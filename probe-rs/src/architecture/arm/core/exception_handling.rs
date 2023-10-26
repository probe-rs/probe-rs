//! This module contains the implementation of the [`crate::core::ExceptionInterface`] for the various ARM core variants.

use crate::{
    core::{ExceptionInfo, ExceptionInterface},
    debug::DebugRegisters,
    Error, MemoryInterface,
};
pub(crate) mod armv6m;
/// Where applicable, this defines shared logic for implementing exception handling accross the various ARMv6-m and ARMv7-m [`crate::CoreType`]'s.
pub(crate) mod armv6m_armv7m_shared;
// NOTE: There is also a [`CoreType::Armv7em`] variant, but it is not currently used/implemented in probe-rs.
pub(crate) mod armv7m;

pub(crate) mod armv8m;

/// Exception handling for cores based on the ARMv6-M architecture.
pub struct ArmV6MExceptionHandler {}

impl ExceptionInterface for ArmV6MExceptionHandler {
    fn exception_details(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
    ) -> Result<Option<ExceptionInfo>, Error> {
        armv6m_armv7m_shared::exception_details(self, memory_interface, stackframe_registers)
    }

    fn calling_frame_registers(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<crate::debug::DebugRegisters, crate::Error> {
        armv6m_armv7m_shared::calling_frame_registers(memory_interface, stackframe_registers)
    }

    fn exception_description(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<String, crate::Error> {
        crate::architecture::arm::core::exception_handling::armv6m::exception_description(
            memory_interface,
            stackframe_registers,
        )
    }
}

/// Exception handling for cores based on the ARMv7-M and ARMv7-EM architectures.
pub struct ArmV7MExceptionHandler {}

impl ExceptionInterface for ArmV7MExceptionHandler {
    fn exception_details(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
    ) -> Result<Option<ExceptionInfo>, Error> {
        armv6m_armv7m_shared::exception_details(self, memory_interface, stackframe_registers)
    }

    fn calling_frame_registers(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<crate::debug::DebugRegisters, crate::Error> {
        armv6m_armv7m_shared::calling_frame_registers(memory_interface, stackframe_registers)
    }

    fn exception_description(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<String, crate::Error> {
        // Load the provided xPSR register as a bitfield.
        let exception_number = armv6m_armv7m_shared::Xpsr(
            stackframe_registers
                .get_register_value_by_role(&crate::core::RegisterRole::ProcessorStatus)?
                as u32,
        )
        .exception_number();

        Ok(format!(
            "{:?}",
            armv7m::ExceptionReason::from(exception_number)
                .expanded_description(memory_interface)?
        ))
    }
}
