//! This module (and its children) contains the implementation of the [`ExceptionInterface`] for the various ARM core
//! variants.

use probe_rs_target::CoreType;

use crate::MemoryInterface;

use super::{DebugError, DebugInfo, DebugRegisters, StackFrame};

pub(crate) mod armv6m;
/// Where applicable, this defines shared logic for implementing exception handling across the various ARMv6-m and
/// ARMv7-m [`crate::CoreType`]'s.
pub(crate) mod armv6m_armv7m_shared;
// NOTE: There is also a [`CoreType::Armv7em`] variant, but it is not currently used/implemented in probe-rs.
pub(crate) mod armv7m;

pub(crate) mod armv8m;

/// Creates a new exception interface for the [`CoreType`] at hand.
pub fn exception_handler_for_core(core_type: CoreType) -> Box<dyn ExceptionInterface> {
    use self::{armv6m, armv7m, armv8m};
    match core_type {
        CoreType::Armv6m => Box::new(armv6m::ArmV6MExceptionHandler),
        CoreType::Armv7m | CoreType::Armv7em => Box::new(armv7m::ArmV7MExceptionHandler),
        CoreType::Armv8m => Box::new(armv8m::ArmV8MExceptionHandler),
        CoreType::Armv7a | CoreType::Armv8a | CoreType::Riscv | CoreType::Xtensa => {
            Box::new(UnimplementedExceptionHandler)
        }
    }
}

/// Placeholder for exception handling for cores where handling exceptions is not yet supported.
pub struct UnimplementedExceptionHandler;

impl ExceptionInterface for UnimplementedExceptionHandler {
    fn exception_details(
        &self,
        _memory: &mut dyn MemoryInterface,
        _stackframe_registers: &DebugRegisters,
        _debug_info: &DebugInfo,
    ) -> Result<Option<ExceptionInfo>, DebugError> {
        // For architectures where the exception handling has not been implemented in probe-rs,
        // this will result in maintaining the current `unwind` behavior, i.e. unwinding will include up
        // to the first frame that was called from an exception handler.
        Ok(None)
    }

    fn calling_frame_registers(
        &self,
        _memory: &mut dyn MemoryInterface,
        _stackframe_registers: &crate::debug::DebugRegisters,
        _raw_exception: u32,
    ) -> Result<crate::debug::DebugRegisters, DebugError> {
        Err(DebugError::NotImplemented("calling frame registers"))
    }

    fn raw_exception(
        &self,
        _stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<u32, DebugError> {
        Err(DebugError::NotImplemented(
            "Not implemented for this architecture.",
        ))
    }

    fn exception_description(
        &self,
        _raw_exception: u32,
        _memory: &mut dyn MemoryInterface,
    ) -> Result<String, DebugError> {
        Err(DebugError::NotImplemented("exception description"))
    }
}

/// A struct containing key information about an exception.
/// The exception details are architecture specific, and the abstraction is handled in the
/// architecture specific implementations of [`ExceptionInterface`].
#[derive(PartialEq)]
pub struct ExceptionInfo {
    /// The exception number.
    /// This is architecture specific and can be used to decode the architecture specific exception reason.
    pub raw_exception: u32,
    /// A human readable explanation for the exception.
    pub description: String,
    /// A populated [`StackFrame`] to represent the stack data in the exception handler.
    pub handler_frame: StackFrame,
}

/// A generic interface to identify and decode exceptions during unwind processing.
pub trait ExceptionInterface {
    /// Using the `stackframe_registers` for a "called frame",
    /// determine if the given frame was called from an exception handler,
    /// and resolve the relevant details about the exception, including the reason for the exception,
    /// and the stackframe registers for the frame that triggered the exception.
    /// A return value of `Ok(None)` indicates that the given frame was called from within the current thread,
    /// and the unwind should continue normally.
    fn exception_details(
        &self,
        memory: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
        debug_info: &DebugInfo,
    ) -> Result<Option<ExceptionInfo>, DebugError>;

    /// Using the `stackframe_registers` for a "called frame", retrieve updated register values for the "calling frame".
    fn calling_frame_registers(
        &self,
        memory: &mut dyn MemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
        raw_exception: u32,
    ) -> Result<crate::debug::DebugRegisters, DebugError>;

    /// Retrieve the architecture specific exception number.
    fn raw_exception(
        &self,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<u32, DebugError>;

    /// Convert the architecture specific exception number into a human readable description.
    /// Where possible, the implementation may read additional registers from the core, to provide additional context.
    fn exception_description(
        &self,
        raw_exception: u32,
        memory: &mut dyn MemoryInterface,
    ) -> Result<String, DebugError>;
}
