//! Sequences for STM32F-series devices

use std::sync::Arc;

use super::ArmDebugSequence;
use crate::architecture::arm::{
    ap::MemoryAp, component::TraceSink, memory::CoresightComponent, ApAddress, ArmError,
    ArmProbeInterface, DpAddress,
};

/// Marker structure for STM32F-series devices.
pub struct Stm32fSeries {}

impl Stm32fSeries {
    /// Create the sequencer for the F-series family of parts.
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }
}

mod dbgmcu {
    use crate::architecture::arm::{memory::adi_v5_memory_interface::ArmProbe, ArmError};
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
        pub fn read(memory: &mut dyn ArmProbe) -> Result<Self, ArmError> {
            let contents = memory.read_word_32(DBGMCU + Self::ADDRESS)?;
            Ok(Self(contents))
        }

        /// Write the control register to memory.
        pub fn write(&mut self, memory: &mut dyn ArmProbe) -> Result<(), ArmError> {
            memory.write_word_32(DBGMCU + Self::ADDRESS, self.0)
        }
    }
}

impl ArmDebugSequence for Stm32fSeries {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmProbeInterface,
        default_ap: MemoryAp,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(default_ap)?;

        let mut cr = dbgmcu::Control::read(&mut *memory)?;
        cr.enable_standby_debug(true);
        cr.enable_sleep_debug(true);
        cr.enable_stop_debug(true);
        cr.write(&mut *memory)?;

        Ok(())
    }

    fn debug_core_stop(&self, interface: &mut dyn ArmProbeInterface) -> Result<(), ArmError> {
        // Power down the debug components
        let ap = MemoryAp::new(ApAddress {
            dp: DpAddress::Default,
            ap: 0,
        });

        let mut memory = interface.memory_interface(ap)?;

        let mut cr = dbgmcu::Control::read(&mut *memory)?;
        cr.enable_standby_debug(false);
        cr.enable_sleep_debug(false);
        cr.enable_stop_debug(false);
        cr.write(&mut *memory)?;

        Ok(())
    }

    fn trace_start(
        &self,
        interface: &mut dyn ArmProbeInterface,
        components: &[CoresightComponent],
        sink: &TraceSink,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(components[0].ap)?;
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
