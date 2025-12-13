//! Sequences for ARMv8 STM32s: STM32H5, STM32L5, STM32U5
//!
//! This covers devices where DBGMCU is at 0xE004400 and has the TRACE_MODE, TRACE_EN,
//! TRACE_IOEN, DBG_STANDBY, DBG_STOP bits.
//!

use std::sync::Arc;

use probe_rs_target::CoreType;

use crate::architecture::arm::{
    ArmDebugInterface, ArmError, FullyQualifiedApAddress,
    component::TraceSink,
    memory::{ArmMemoryInterface, CoresightComponent},
    sequences::ArmDebugSequence,
};

/// Marker structure for ARMv8 STM32 devices.
#[derive(Debug)]
pub struct Stm32mp2 {}

impl Stm32mp2 {
    /// Create the sequencer for ARMv8 STM32 families.
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }
}

mod dbgmcu {
    use crate::architecture::arm::{ArmError, memory::ArmMemoryInterface};
    use bitfield::bitfield;

    /// The base address of the DBGMCU component
    const DBGMCU: u64 = 0x80010000;

    bitfield! {
        /// The control register (CR) of the DBGMCU. This register is described in "RM0456: STM32U5
        /// family reference manual" section 75.12.4
        pub struct Control(u32);
        impl Debug;

        pub u8, dbg_swd_sel_n, enable_cm0_access: 4;
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

impl ArmDebugSequence for Stm32mp2 {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        {
            let mut memory = interface.memory_interface(default_ap)?;
            let mut cr = dbgmcu::Control::read(&mut *memory)?;
            cr.enable_standby_debug(true);
            cr.enable_stop_debug(true);
            cr.enable_sleep_debug(true);
            // enables CM0 access through the same AP as opposed to seperate SWD pins
            cr.enable_cm0_access(true);
            cr.write(&mut *memory)?;

            memory.write_word_32(0x80210300, 0).unwrap();
            let pre = memory.read_word_32(0x80210088).unwrap();
            memory.write_word_32(0x80210088, pre | 0x0004000).unwrap();

            memory.write_word_32(0x80310300, 0).unwrap();
            let pre = memory.read_word_32(0x80310088).unwrap();
            memory.write_word_32(0x80310088, pre | 0x0004000).unwrap();
        }

        // Power up CM0
        {
            let mut axi_memory =
                interface.memory_interface(&FullyQualifiedApAddress::v1_with_default_dp(4))?;
            axi_memory.write_word_32(0x44200490, 0x0000006).unwrap();
        }

        Ok(())
    }

    fn debug_core_stop(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
    ) -> Result<(), ArmError> {
        let mut cr = dbgmcu::Control::read(&mut *memory)?;
        cr.enable_standby_debug(false);
        cr.enable_stop_debug(false);
        cr.write(&mut *memory)?;

        Ok(())
    }

    fn trace_start(
        &self,
        _interface: &mut dyn ArmDebugInterface,
        _components: &[CoresightComponent],
        _sink: &TraceSink,
    ) -> Result<(), ArmError> {
        // let mut memory = interface.memory_interface(&components[0].ap_address)?;
        // let mut cr = dbgmcu::Control::read(&mut *memory)?;

        // if matches!(sink, TraceSink::Tpiu(_) | TraceSink::Swo(_)) {
        //     cr.set_traceen(true);
        //     cr.set_traceioen(true);
        //     cr.set_tracemode(0);
        // } else {
        //     cr.set_traceen(false);
        //     cr.set_traceioen(false);
        //     cr.set_tracemode(0);
        // }

        // cr.write(&mut *memory)?;
        Err(ArmError::NotImplemented("Tracing not implemented yet"))
    }
}
