use std::ops::ControlFlow;

use crate::{
    DebugError, DebugRegisters, StackFrame, exception_handling::ExceptionInterface,
    unwind_pc_without_debuginfo,
};

use probe_rs::{MemoryInterface, RegisterRole, RegisterValue};

pub struct RiscvExceptionHandler;

#[async_trait::async_trait(?Send)]
impl ExceptionInterface for RiscvExceptionHandler {
    async fn unwind_without_debuginfo(
        &self,
        unwind_registers: &mut DebugRegisters,
        frame_pc: u64,
        _stack_frames: &[StackFrame],
        instruction_set: Option<probe_rs::InstructionSet>,
        memory: &mut dyn MemoryInterface,
    ) -> ControlFlow<Option<DebugError>> {
        // Use the default method to unwind PC.
        unwind_pc_without_debuginfo(unwind_registers, frame_pc, instruction_set)?;

        // Current register values.
        let sp = unwind_registers
            .get_register_value_by_role(&RegisterRole::StackPointer)
            .unwrap();

        if sp < 8 {
            // Stack pointer is too low.
            return ControlFlow::Break(None);
        }

        let mut stack_frame = [0; 2];
        if let Err(e) = memory.read_32(sp - 8, &mut stack_frame).await {
            // FP points at something we can't read.
            return ControlFlow::Break(Some(e.into()));
        }

        let [caller_sp, return_addr] = stack_frame;

        if (caller_sp as u64).saturating_sub(sp) > 0x1000_0000 {
            // Stack pointer is too far away from the current stack pointer.
            return ControlFlow::Break(None);
        }

        let unwound_ra = unwind_registers
            .get_register_mut_by_role(&RegisterRole::ReturnAddress)
            .unwrap();
        unwound_ra.value = Some(RegisterValue::from(return_addr));

        let unwound_sp = unwind_registers
            .get_register_mut_by_role(&RegisterRole::StackPointer)
            .unwrap();
        unwound_sp.value = Some(RegisterValue::from(caller_sp));

        if sp == caller_sp as u64 {
            return ControlFlow::Break(None);
        }

        ControlFlow::Continue(())
    }
}
