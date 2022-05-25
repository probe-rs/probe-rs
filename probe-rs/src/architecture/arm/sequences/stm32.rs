//! Sequences for STM32 devices

use std::sync::Arc;

use super::ArmDebugSequence;
use crate::{
    architecture::arm::{ap::MemoryAp, ApAddress, ArmProbeInterface, DpAddress},
    Memory,
};

/// Marker struct indicating initialization sequencing for STM32H7 family parts.
pub struct Stm32h7 {}

impl Stm32h7 {
    /// Create the sequencer for the H7 family of parts.
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }

    /// Enable all debug components on the chip.
    pub fn enable_debug_components(&self, memory: &mut Memory<'_>) -> Result<(), crate::Error> {
        log::info!("Enabling STM32H7 debug components");
        let mut control = dbgmcu::Control::read(memory)?;

        // Enable the debug clock doamins for D1 and D3. This ensures we can access CoreSight
        // components in these power domains.
        control.enable_d1_clock(true);
        control.enable_d3_clock(true);

        // The TRACECK also has to be enabled to communicate with the TPIU.
        control.enable_traceck(true);

        // Enable debug connection in all power modes.
        control.enable_standby_debug(true);
        control.enable_sleep_debug(true);
        control.enable_stop_debug(true);

        control.write(memory)?;

        Ok(())
    }
}

mod dbgmcu {
    use crate::Memory;
    use bitfield::bitfield;

    /// The base address of the DBGMCU component
    const DBGMCU: u64 = 0xE00E_1000;

    bitfield! {
        pub struct Control(u32);
        impl Debug;

        pub u8, dbgsleep_d1, enable_sleep_debug: 0;
        pub u8, dbgstop_d1, enable_stop_debug: 1;
        pub u8, dbgstby_d1, enable_standby_debug: 2;

        pub u8, d3dbgcken, enable_d3_clock: 22;
        pub u8, d1dbgcken, enable_d1_clock: 21;
        pub u8, traceclken, enable_traceck: 20;
    }

    impl Control {
        /// The offset of the Control register in the DBGMCU block.
        const ADDRESS: u64 = 0x04;

        pub fn read(memory: &mut Memory<'_>) -> Result<Self, crate::Error> {
            let contents = memory.read_word_32(DBGMCU + Self::ADDRESS)?;
            Ok(Self(contents))
        }

        pub fn write(&mut self, memory: &mut Memory<'_>) -> Result<(), crate::Error> {
            memory.write_word_32(DBGMCU + Self::ADDRESS, self.0)
        }
    }
}

impl ArmDebugSequence for Stm32h7 {
    fn debug_device_unlock(
        &self,
        interface: &mut Box<dyn ArmProbeInterface>,
        _default_ap: MemoryAp,
        _permissions: &crate::Permissions,
    ) -> Result<(), crate::Error> {
        // Power up the debug components through AP2, which is the defualt AP debug port.
        let ap = MemoryAp::new(ApAddress {
            dp: DpAddress::Default,
            ap: 2,
        });

        let mut memory = interface.memory_interface(ap)?;
        self.enable_debug_components(&mut memory)?;

        Ok(())
    }
}
