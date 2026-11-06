//! Sequences for STM32WL3 devices.
//!
//! WL3 is a Cortex-M0+ based device with a distinct memory map: DBGMCU is at
//! 0x40008000 and RCC at 0x48400000. The DBG_CR register exposes DBG_STOP
//! (bit 1) and DBG_SLEEP (bit 0) — there is no DBG_STANDBY bit. See RM0511
//! section 30.3.1 and section 6.6.13.

use std::{sync::Arc, thread, time::Duration};

use probe_rs_target::CoreType;

use crate::architecture::arm::{
    ArmDebugInterface, ArmError, FullyQualifiedApAddress, memory::ArmMemoryInterface,
    sequences::ArmDebugSequence,
};

/// Marker structure for STM32WL3 devices.
#[derive(Debug)]
pub struct Stm32Wl3;

impl Stm32Wl3 {
    /// Create the sequencer for STM32WL3 devices.
    pub fn create() -> Arc<Self> {
        Arc::new(Self)
    }
}

mod rcc {
    use crate::architecture::arm::{ArmError, memory::ArmMemoryInterface};
    use bitfield::bitfield;

    /// The base address of the RCC peripheral
    const RCC: u64 = 0x4840_0000;

    bitfield! {
        /// The APB0 clock enable register (APB0ENR) of the RCC. This register is described in
        /// "RM0511: STM32WL30xx/31xx/33xx reference manual" section 6.6.13.
        pub struct Apb0Enr(u32);
        impl Debug;

        pub u8, dbgmcuen, enable_dbgmcu: 15;
    }

    impl Apb0Enr {
        /// The offset of the APB0ENR register in the RCC block.
        const ADDRESS: u64 = 0x54;

        /// Read the enable register from memory.
        pub fn read(memory: &mut dyn ArmMemoryInterface) -> Result<Self, ArmError> {
            let contents = memory.read_word_32(RCC + Self::ADDRESS)?;
            Ok(Self(contents))
        }

        /// Write the enable register to memory.
        pub fn write(&mut self, memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
            memory.write_word_32(RCC + Self::ADDRESS, self.0)
        }
    }
}

mod dbgmcu {
    use crate::architecture::arm::{ArmError, memory::ArmMemoryInterface};
    use bitfield::bitfield;

    /// The base address of the DBGMCU component
    const DBGMCU: u64 = 0x4000_8000;

    bitfield! {
        /// The configuration register (CR) of the DBGMCU. This register is described in
        /// "RM0511: STM32WL30xx/31xx/33xx reference manual" section 30.3.1.
        pub struct Control(pub u32);
        impl Debug;

        pub u8, dbg_stop, enable_stop_debug: 1;
        pub u8, dbg_sleep, enable_sleep_debug: 0;
    }

    impl Control {
        /// The offset of the Control register in the DBGMCU block.
        const ADDRESS: u64 = 0x00;

        /// Read the control register from memory.
        pub fn read(memory: &mut dyn ArmMemoryInterface) -> Result<Self, ArmError> {
            let contents = memory.read_word_32(DBGMCU + Self::ADDRESS)?;
            Ok(Self(contents))
        }

        /// Write the control register to memory.
        pub fn write(&mut self, memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
            memory.write_word_32(DBGMCU + Self::ADDRESS, self.0)
        }
    }
}

impl ArmDebugSequence for Stm32Wl3 {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(default_ap)?;

        let mut enr = rcc::Apb0Enr::read(&mut *memory)?;
        enr.enable_dbgmcu(true);
        enr.write(&mut *memory)?;

        let mut cr = dbgmcu::Control::read(&mut *memory)?;
        cr.enable_stop_debug(true);
        cr.enable_sleep_debug(true);
        cr.write(&mut *memory)?;

        Ok(())
    }

    fn debug_core_stop(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
    ) -> Result<(), ArmError> {
        self.debug_core_stop_default(memory, core_type)?;

        let mut cr = dbgmcu::Control::read(&mut *memory)?;
        cr.enable_stop_debug(false);
        cr.enable_sleep_debug(false);
        cr.write(&mut *memory)?;

        let mut enr = rcc::Apb0Enr::read(&mut *memory)?;
        enr.enable_dbgmcu(false);
        enr.write(&mut *memory)?;

        Ok(())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // WL3 only re-samples the BOOT configuration on a hardware reset. A plain
        // AIRCR SYSRESETREQ clears the flash controller's REMAP bit, after which
        // the main flash firmware runs; if that firmware enters DeepStop the SWD
        // port is unpowered and all subsequent debug access fails. Pulse nRST to
        // force the BOOT pins to be re-sampled, then rebuild the debug session.
        let ap = interface.fully_qualified_address();
        let arm_interface = interface.get_arm_debug_interface()?;

        let probe = arm_interface.driver_mut().ok_or(ArmError::NotImplemented(
            "driver_mut (required to pulse nRST on STM32WL3)",
        ))?;
        probe.target_reset_assert().map_err(ArmError::Probe)?;
        thread::sleep(Duration::from_millis(50));
        probe.target_reset_deassert().map_err(ArmError::Probe)?;
        thread::sleep(Duration::from_millis(100));

        arm_interface.reinitialize()?;
        self.debug_core_start(arm_interface, &ap, core_type, debug_base, None)?;

        let mut memory = arm_interface.memory_interface(&ap)?;

        let mut enr = rcc::Apb0Enr::read(&mut *memory)?;
        enr.enable_dbgmcu(true);
        enr.write(&mut *memory)?;

        let mut cr = dbgmcu::Control::read(&mut *memory)?;
        cr.enable_stop_debug(true);
        cr.enable_sleep_debug(true);
        cr.write(&mut *memory)?;

        Ok(())
    }
}
