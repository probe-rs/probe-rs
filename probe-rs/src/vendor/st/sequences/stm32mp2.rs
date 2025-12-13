//! Sequences for STM32MP2 devices

use std::sync::Arc;

use probe_rs_target::CoreType;

use crate::architecture::arm::{
    ArmDebugInterface, ArmError, FullyQualifiedApAddress,
    component::TraceSink,
    memory::{ArmMemoryInterface, CoresightComponent},
    sequences::ArmDebugSequence,
};

/// DAP for the APB debug bus going to both A35s
pub const STM32MP2_CA35_AP: u8 = 0;
/// DAP for the CM33
pub const STM32MP2_CM33_AP: u8 = 8;
/// DAP for the CM0P
pub const STM32MP2_CM0P_AP: u8 = 2;
/// DAP for the AXI Bus Matrix
pub const STM32MP2_AXI_AP: u8 = 4;
/// DAP for the AHB SmartRun Bus Matrix
pub const STM32MP2_SR_AHB_AP: u8 = 1;

/// Marker structure for ARMv8 STM32 devices.
#[derive(Debug, PartialEq, Eq)]
pub enum Stm32mp2Line {
    /// STM32MP251/253/255/257 Have A35+M33+M0p
    MP25,
    /// STM32MP233/235/237 Have 2xA35+M33
    MP23,
}

/// Marker structure for ARMv8 STM32 devices.
#[derive(Debug)]
pub struct Stm32mp2 {
    line: Stm32mp2Line,
}

impl Stm32mp2 {
    /// Create the sequencer for ARMv8 STM32 families.
    pub fn create(line: Stm32mp2Line) -> Arc<Self> {
        Arc::new(Self { line })
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
        _default_ap: &FullyQualifiedApAddress,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        {
            let mut memory = interface.memory_interface(
                &FullyQualifiedApAddress::v1_with_default_dp(STM32MP2_CA35_AP),
            )?;
            let mut cr = dbgmcu::Control::read(&mut *memory)?;
            cr.enable_standby_debug(true);
            cr.enable_stop_debug(true);
            cr.enable_sleep_debug(true);
            // enables CM0 access through the same AP as opposed to seperate SWD pins
            cr.enable_cm0_access(true);
            cr.write(&mut *memory)?;
        }

        // Power up CM0 if chip has it
        if self.line == Stm32mp2Line::MP25 {
            let mut axi_memory = interface.memory_interface(
                &FullyQualifiedApAddress::v1_with_default_dp(STM32MP2_AXI_AP),
            )?;
            // Enable MSI Clock
            axi_memory.write_word_32(0x44200444, 0x0000003).unwrap();
            // Enable CM0P
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
        interface: &mut dyn ArmDebugInterface,
        _components: &[CoresightComponent],
        sink: &TraceSink,
    ) -> Result<(), ArmError> {
        let mut axi_memory = interface.memory_interface(
            &FullyQualifiedApAddress::v1_with_default_dp(STM32MP2_AXI_AP),
        )?;
        let pre = axi_memory.read_word_32(0x44200520)?;

        if matches!(sink, TraceSink::Tpiu(_) | TraceSink::Swo(_)) {
            axi_memory.write_word_32(0x44200520, pre | 0x200)?;
        } else {
            axi_memory.write_word_32(0x44200520, pre & !0x200)?;
        }

        Ok(())
    }
}
