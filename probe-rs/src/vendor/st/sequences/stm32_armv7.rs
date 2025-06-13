//! Sequences for most ARMv7 STM32s: STM32F1/2/3/4/7, STM32G4, STM32L1/4, STM32WB and STM32WL.
//!
//! This covers devices where DBGMCU is at 0xE0042000 and has the TRACE_MODE, TRACE_IOEN,
//! DBG_STANDBY, DBG_STOP, and DBG_SLEEP bits, which is most STM32 devices with ARMv7 CPUs.
//!
//! It does _not_ include STM32F0, STM32G0, STM32L0, which are ARMv6 and have a simpler DBGMCU
//! component at a different address which requires clock gating, or the STM32L5 or STM32U5 which
//! are ARMv8, or the STM32H7 which is ARMv7 but has a more complicated DBGMCU at a different
//! address.

use std::sync::{Arc, Mutex};

use probe_rs_target::CoreType;

use crate::architecture::arm::{
    ArmError, ArmDebugInterface, FullyQualifiedApAddress,
    component::TraceSink,
    memory::{ArmMemoryInterface, CoresightComponent},
    sequences::ArmDebugSequence,
};

/// Marker structure for most ARMv7 STM32 devices.
#[derive(Debug)]
pub struct Stm32Armv7 {
    saved_cr_value: Mutex<Option<u32>>,
}

impl Stm32Armv7 {
    /// Create the sequencer for most ARMv7 STM32 families.
    pub fn create() -> Arc<Self> {
        Arc::new(Self {
            saved_cr_value: Mutex::new(None),
        })
    }
}

mod dbgmcu {
    use crate::architecture::arm::{ArmError, memory::ArmMemoryInterface};
    use bitfield::bitfield;

    /// The base address of the DBGMCU component
    const DBGMCU: u64 = 0xE004_2000;

    bitfield! {
        /// The control register (CR) of the DBGMCU. This register is described in "RM0090: STM32F7
        /// family reference manual" section 38.16.3
        pub struct Control(u32);
        impl Debug;

        pub u8, trace_mode, set_tracemode: 7, 6;
        pub u8, trace_ioen, set_traceioen: 5;
        pub u8, dbg_standby, enable_standby_debug: 2;
        pub u8, dbg_stop, enable_stop_debug: 1;
        pub u8, dbg_sleep, enable_sleep_debug: 0;
    }

    impl Control {
        /// The offset of the Control register in the DBGMCU block.
        const ADDRESS: u64 = 0x04;

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

impl ArmDebugSequence for Stm32Armv7 {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(default_ap)?;
        let mut cr = dbgmcu::Control::read(&mut *memory)?;
        self.saved_cr_value.lock().unwrap().replace(cr.0);

        cr.enable_standby_debug(true);
        cr.enable_sleep_debug(true);
        cr.enable_stop_debug(true);
        cr.write(&mut *memory)?;

        Ok(())
    }

    fn debug_core_stop(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
    ) -> Result<(), ArmError> {
        let mut saved_cr_value = self.saved_cr_value.lock().unwrap();
        if let Some(crv) = saved_cr_value.take() {
            let mut cr = dbgmcu::Control(crv);
            cr.write(&mut *memory)?;
        }
        Ok(())
    }

    fn trace_start(
        &self,
        interface: &mut dyn ArmDebugInterface,
        components: &[CoresightComponent],
        sink: &TraceSink,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(&components[0].ap_address)?;
        let mut cr = dbgmcu::Control::read(&mut *memory)?;

        if matches!(sink, TraceSink::Tpiu(_) | TraceSink::Swo(_)) {
            cr.set_traceioen(true);
            cr.set_tracemode(0);
        } else {
            cr.set_traceioen(false);
            cr.set_tracemode(0);
        }

        cr.write(&mut *memory)?;
        Ok(())
    }
}
