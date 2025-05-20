//! Sequences for ARMv8 STM32s: STM32H5, STM32L5, STM32U5
//!
//! This covers devices where DBGMCU is at 0xE004400 and has the TRACE_MODE, TRACE_EN,
//! TRACE_IOEN, DBG_STANDBY, DBG_STOP bits.
//!

use std::sync::Arc;

use probe_rs_target::CoreType;

use crate::architecture::arm::{
    ArmError, ArmProbeInterface, FullyQualifiedApAddress,
    component::TraceSink,
    memory::{ArmMemoryInterface, CoresightComponent},
    sequences::ArmDebugSequence,
};

/// Marker structure for ARMv8 STM32 devices.
#[derive(Debug)]
pub struct Stm32Armv8 {}

impl Stm32Armv8 {
    /// Create the sequencer for ARMv8 STM32 families.
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }
}

mod dbgmcu {
    use crate::architecture::arm::{ArmError, memory::ArmMemoryInterface};
    use bitfield::bitfield;

    /// The base address of the DBGMCU component
    const DBGMCU: u64 = 0xE0044000;

    bitfield! {
        /// The control register (CR) of the DBGMCU. This register is described in "RM0456: STM32U5
        /// family reference manual" section 75.12.4
        pub struct Control(u32);
        impl Debug;

        pub u8, trace_mode, set_tracemode: 7, 6;
        pub u8, trace_en, set_traceen: 5;
        pub u8, trace_ioen, set_traceioen: 4;
        pub u8, dbg_standby, enable_standby_debug: 2;
        pub u8, dbg_stop, enable_stop_debug: 1;
    }

    impl Control {
        /// The offset of the Control register in the DBGMCU block.
        const ADDRESS: u64 = 0x04;

        /// Read the control register from memory.
        pub async fn read(memory: &mut dyn ArmMemoryInterface) -> Result<Self, ArmError> {
            let contents = memory.read_word_32(DBGMCU + Self::ADDRESS).await?;
            Ok(Self(contents))
        }

        /// Write the control register to memory.
        pub async fn write(&mut self, memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
            memory.write_word_32(DBGMCU + Self::ADDRESS, self.0).await
        }
    }
}

#[async_trait::async_trait(?Send)]
impl ArmDebugSequence for Stm32Armv8 {
    async fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmProbeInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(default_ap).await?;
        let mut cr = dbgmcu::Control::read(&mut *memory).await?;
        cr.enable_standby_debug(true);
        cr.enable_stop_debug(true);
        cr.write(&mut *memory).await?;

        Ok(())
    }

    async fn debug_core_stop(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
    ) -> Result<(), ArmError> {
        let mut cr = dbgmcu::Control::read(&mut *memory).await?;
        cr.enable_standby_debug(false);
        cr.enable_stop_debug(false);
        cr.write(&mut *memory).await?;

        Ok(())
    }

    async fn trace_start(
        &self,
        interface: &mut dyn ArmProbeInterface,
        components: &[CoresightComponent],
        sink: &TraceSink,
    ) -> Result<(), ArmError> {
        let mut memory = interface
            .memory_interface(&components[0].ap_address)
            .await?;
        let mut cr = dbgmcu::Control::read(&mut *memory).await?;

        if matches!(sink, TraceSink::Tpiu(_) | TraceSink::Swo(_)) {
            cr.set_traceen(true);
            cr.set_traceioen(true);
            cr.set_tracemode(0);
        } else {
            cr.set_traceen(false);
            cr.set_traceioen(false);
            cr.set_tracemode(0);
        }

        cr.write(&mut *memory).await?;
        Ok(())
    }
}
