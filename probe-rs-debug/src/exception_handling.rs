//! This module (and its children) contains the implementation of the [`ExceptionInterface`] for the various ARM core
//! variants.

use std::ops::ControlFlow;

use probe_rs_target::CoreType;

use crate::unwind_pc_without_debuginfo;
use probe_rs::{
    CoreRegister, InstructionSet, MemoryInterface, RegisterDataType, RegisterRole, RegisterValue,
    UnwindRule,
};

use super::{DebugError, DebugInfo, DebugRegisters, StackFrame};

pub(crate) mod armv6m;
/// Where applicable, this defines shared logic for implementing exception handling across the various ARMv6-m and
/// ARMv7-m [`crate::CoreType`]'s.
pub(crate) mod armv6m_armv7m_shared;
// NOTE: There is also a [`CoreType::Armv7em`] variant, but it is not currently used/implemented in probe-rs.
pub(crate) mod armv7m;
pub(crate) mod armv8m;
pub(crate) mod riscv;
pub(crate) mod xtensa;

/// Creates a new exception interface for the [`CoreType`] at hand.
pub fn exception_handler_for_core(core_type: CoreType) -> Box<dyn ExceptionInterface> {
    use self::{armv6m, armv7m, armv8m};
    match core_type {
        CoreType::Armv6m => Box::new(armv6m::ArmV6MExceptionHandler),
        CoreType::Armv7m | CoreType::Armv7em => Box::new(armv7m::ArmV7MExceptionHandler),
        CoreType::Armv8m => Box::new(armv8m::ArmV8MExceptionHandler),
        CoreType::Xtensa => Box::<xtensa::XtensaExceptionHandler>::default(),
        CoreType::Riscv | CoreType::Riscv64 => Box::new(riscv::RiscvExceptionHandler),
        CoreType::Armv7a | CoreType::Armv7r | CoreType::Armv8a => {
            Box::new(UnimplementedExceptionHandler)
        }
    }
}

/// Placeholder for exception handling for cores where handling exceptions is not yet supported.
pub struct UnimplementedExceptionHandler;

impl ExceptionInterface for UnimplementedExceptionHandler {}

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
        _memory: &mut dyn MemoryInterface,
        _stackframe_registers: &DebugRegisters,
        _debug_info: &DebugInfo,
    ) -> Result<Option<ExceptionInfo>, DebugError> {
        // For architectures where the exception handling has not been implemented in probe-rs,
        // this will result in maintaining the current `unwind` behavior, i.e. unwinding will include up
        // to the first frame that was called from an exception handler.
        Ok(None)
    }

    /// Using the `stackframe_registers` for a "called frame", retrieve updated register values for the "calling frame".
    fn calling_frame_registers(
        &self,
        _memory: &mut dyn MemoryInterface,
        _stackframe_registers: &crate::DebugRegisters,
        _raw_exception: u32,
    ) -> Result<crate::DebugRegisters, DebugError> {
        Err(DebugError::NotImplemented("calling frame registers"))
    }

    /// Retrieve the architecture specific exception number.
    fn raw_exception(
        &self,
        _stackframe_registers: &crate::DebugRegisters,
    ) -> Result<u32, DebugError> {
        Err(DebugError::NotImplemented("raw exception"))
    }

    /// Convert the architecture specific exception number into a human readable description.
    /// Where possible, the implementation may read additional registers from the core, to provide additional context.
    fn exception_description(
        &self,
        _raw_exception: u32,
        _memory: &mut dyn MemoryInterface,
    ) -> Result<String, DebugError> {
        Err(DebugError::NotImplemented("exception description"))
    }

    /// Unwind the stack without debug info.
    ///
    /// This method can be implemented to provide a stack trace using frame pointers, for example.
    fn unwind_without_debuginfo(
        &self,
        unwind_registers: &mut DebugRegisters,
        frame_pc: u64,
        _stack_frames: &[StackFrame],
        instruction_set: Option<InstructionSet>,
        _memory: &mut dyn MemoryInterface,
    ) -> ControlFlow<Option<DebugError>> {
        unwind_pc_without_debuginfo(unwind_registers, frame_pc, instruction_set)
    }

    /// Compute the caller-frame value of `debug_register` when DWARF has no rule for it.
    ///
    /// `register_rule` is updated with a short description of the rule that was applied,
    /// for inclusion in unwind trace logs.
    ///
    /// In many cases, the DWARF has `Undefined` rules for variables like frame pointer, program counter, etc.,
    /// so the default implementation hard-codes ARM/RISC-V style heuristics here to make sure unwinding can continue.
    /// If there is a valid rule, it will bypass these hardcoded ones.
    fn unwind_undefined_register(
        &self,
        debug_register: &CoreRegister,
        callee_frame_registers: &DebugRegisters,
        unwind_cfa: Option<u64>,
        _memory: &mut dyn MemoryInterface,
        register_rule: &mut String,
    ) -> Result<Option<RegisterValue>, DebugError> {
        if debug_register.register_has_role(RegisterRole::FramePointer) {
            *register_rule = "FP=CFA (dwarf Undefined)".to_string();
            return Ok(cfa_as_register(debug_register, unwind_cfa));
        }

        if debug_register.register_has_role(RegisterRole::StackPointer) {
            // NOTE: [ARMv7-M Architecture Reference Manual](https://developer.arm.com/documentation/ddi0403/ee), Section B.1.4.1: Treat bits [1:0] as `Should be Zero or Preserved`
            // - Applying this logic to RISC-V has no adverse effects, since all incoming addresses are already 32-bit aligned.
            *register_rule = "SP=CFA (dwarf Undefined)".to_string();
            return Ok(cfa_as_register(debug_register, unwind_cfa));
        }

        if debug_register.register_has_role(RegisterRole::ReturnAddress) {
            let current_pc = callee_frame_registers
                .get_register_value_by_role(&RegisterRole::ProgramCounter)
                .map_err(|_| {
                    DebugError::Other(
                        "UNWIND: Tried to unwind return address value where current program counter is unknown."
                            .to_string(),
                    )
                })?;
            let current_lr = callee_frame_registers
                .get_register_by_role(&RegisterRole::ReturnAddress)
                .ok()
                .and_then(|lr| lr.value)
                .ok_or_else(|| {
                    DebugError::Other(
                        "UNWIND: Tried to unwind return address value where current return address is unknown."
                            .to_string(),
                    )
                })?;

            let current_lr_value: u64 = current_lr.try_into()?;

            return Ok(if current_pc == current_lr_value & !0b1 {
                // If the previous PC is the same as the half-word aligned current LR,
                // we have no way of inferring the previous frames LR until we have the PC.
                *register_rule = "LR=Undefined (dwarf Undefined)".to_string();
                None
            } else {
                // We can attempt to continue unwinding with the current LR value, e.g. inlined code.
                *register_rule = "LR=Current LR (dwarf Undefined)".to_string();
                Some(current_lr)
            });
        }

        if debug_register.register_has_role(RegisterRole::ProgramCounter) {
            unreachable!("The program counter is handled separately")
        }

        // If the register rule was not specified, then we either carry the previous value forward,
        // or we clear the register value, depending on the architecture and register type.
        Ok(match debug_register.unwind_rule {
            UnwindRule::Preserve => {
                *register_rule = "Preserve".to_string();
                callee_frame_registers
                    .get_register(debug_register.id)
                    .and_then(|reg| reg.value)
            }
            UnwindRule::Clear => {
                *register_rule = "Clear".to_string();
                None
            }
            UnwindRule::SpecialRule => {
                // When no DWARF rules are available, and it is not a special register like PC, SP, FP, etc.,
                // we will clear the value. It is possible it might have its value set later if
                // exception frame information is available.
                *register_rule = "Clear (no unwind rules specified)".to_string();
                None
            }
        })
    }
}

fn cfa_as_register(
    debug_register: &CoreRegister,
    unwind_cfa: Option<u64>,
) -> Option<RegisterValue> {
    unwind_cfa.map(|cfa| {
        if debug_register.data_type == RegisterDataType::UnsignedInteger(32) {
            RegisterValue::U32(cfa as u32 & !0b11)
        } else {
            RegisterValue::U64(cfa & !0b11)
        }
    })
}
