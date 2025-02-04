use std::ops::ControlFlow;

use crate::{
    exception_handling::{ExceptionInfo, ExceptionInterface},
    unwind_pc_without_debuginfo, DebugError, DebugInfo, DebugRegisters, StackFrame,
};

use probe_rs::{MemoryInterface, RegisterRole, RegisterValue};

pub struct XtensaExceptionHandler;

impl ExceptionInterface for XtensaExceptionHandler {
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
        _stackframe_registers: &crate::DebugRegisters,
        _raw_exception: u32,
    ) -> Result<crate::DebugRegisters, DebugError> {
        Err(DebugError::NotImplemented("calling frame registers"))
    }

    fn raw_exception(
        &self,
        _stackframe_registers: &crate::DebugRegisters,
    ) -> Result<u32, DebugError> {
        Err(DebugError::NotImplemented("raw exception"))
    }

    fn exception_description(
        &self,
        _raw_exception: u32,
        _memory: &mut dyn MemoryInterface,
    ) -> Result<String, DebugError> {
        Err(DebugError::NotImplemented("exception description"))
    }

    fn unwind_without_debuginfo(
        &self,
        unwind_registers: &mut DebugRegisters,
        frame_pc: u64,
        _stack_frames: &[StackFrame],
        instruction_set: Option<probe_rs::InstructionSet>,
        memory: &mut dyn MemoryInterface,
    ) -> ControlFlow<Option<DebugError>> {
        // Use the default method to unwind PC.
        unwind_pc_without_debuginfo(unwind_registers, frame_pc, instruction_set)?;

        // We can try and use FP to unwind SP and RA that allows us to continue unwinding.

        // Current register values
        let Ok(fp) = unwind_registers.get_register_value_by_role(&RegisterRole::FramePointer)
        else {
            // We can't unwind without FP.
            return ControlFlow::Break(None);
        };

        let sp = unwind_registers
            .get_register_value_by_role(&RegisterRole::StackPointer)
            .unwrap();

        if sp.abs_diff(fp) >= 1024 * 1024 {
            // Heuristic: the stack frame is probably smaller than 1MB.
            return ControlFlow::Continue(());
        }

        // Read PC and FP from previous stack frame's Register-Spill Area.
        let mut stack_frame = [0; 2];
        if let Err(e) = memory.read_32(fp - 16, &mut stack_frame) {
            // FP points at something we can't read.
            return ControlFlow::Break(Some(e.into()));
        }

        let [caller_ra, caller_sp] = stack_frame;

        let unwound_fp = unwind_registers
            .get_register_mut_by_role(&RegisterRole::FramePointer)
            .unwrap();
        unwound_fp.value = Some(RegisterValue::from(caller_sp));

        let unwound_ra = unwind_registers
            .get_register_mut_by_role(&RegisterRole::ReturnAddress)
            .unwrap();
        unwound_ra.value = Some(RegisterValue::from(caller_ra));

        ControlFlow::Continue(())
    }
}
