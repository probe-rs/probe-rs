use std::ops::ControlFlow;

use crate::{
    DebugError, DebugRegisters, StackFrame, exception_handling::ExceptionInterface,
    unwind_pc_without_debuginfo,
};

use probe_rs::{InstructionSet, MemoryInterface, RegisterRole, RegisterValue};

pub struct RiscvExceptionHandler;

impl RiscvExceptionHandler {
    fn unwind_registers(
        &self,
        memory: &mut dyn MemoryInterface,
        unwind_registers: &mut DebugRegisters,
    ) -> Result<(), DebugError> {
        let ra = unwind_registers.get_register_value_by_role(&RegisterRole::ReturnAddress)?;
        if ra == 0 {
            return Ok(());
        }

        // Current register values.
        let sp = unwind_registers.get_register_value_by_role(&RegisterRole::StackPointer)?;

        if sp < 8 {
            // Stack pointer is too low, cannot unwind.
            return Err(DebugError::Other(
                "Stack pointer is too low to unwind".to_string(),
            ));
        }

        // The callee saved the frame pointer of the current frame at the bottom of its own frame.
        // The frame pointer is the address above the current frame, which is the stack pointer of
        // the caller.
        let caller_sp = memory.read_word_32(sp - 8)? as u64;

        if caller_sp <= sp {
            // The stack grows down, so the stack pointer of the caller must be above.
            return Err(DebugError::Other(
                "Stack pointer of the caller is not above the current stack pointer".to_string(),
            ));
        }

        // TODO: use an architecture-appropriate value?
        if caller_sp - sp > 0x1000_0000 {
            // Stack pointer is too far away from the current stack pointer.
            return Err(DebugError::Other(
                "Stack pointer is too far away to unwind".to_string(),
            ));
        }

        // The current frame stored the return address and the frame pointer of the caller at the
        // top of its own frame.
        let mut stack_frame = [0; 2];
        memory.read_32(caller_sp - 8, &mut stack_frame)?;

        let [caller_fp, return_addr] = stack_frame;

        // TODO: unwind other registers as well.
        let regs_from_current_frame = [
            (RegisterRole::ReturnAddress, return_addr),
            (RegisterRole::StackPointer, caller_sp as u32),
            (RegisterRole::FramePointer, caller_fp),
        ];

        for (role, value) in regs_from_current_frame {
            let reg = unwind_registers.get_register_mut_by_role(&role).unwrap();
            reg.value = Some(RegisterValue::from(value));
        }

        Ok(())
    }
}

impl ExceptionInterface for RiscvExceptionHandler {
    fn unwind_without_debuginfo(
        &self,
        unwind_registers: &mut DebugRegisters,
        frame_pc: u64,
        _stack_frames: &[StackFrame],
        instruction_set: Option<InstructionSet>,
        memory: &mut dyn MemoryInterface,
    ) -> ControlFlow<Option<DebugError>> {
        // The return address must be unwound first, because the program counter of the calling
        // frame comes from it.
        // TODO: this should be automatically handled by the caller.
        if let Err(error) = self.unwind_registers(memory, unwind_registers) {
            return ControlFlow::Break(Some(error));
        }

        // Use the default method to unwind PC.
        unwind_pc_without_debuginfo(unwind_registers, frame_pc, instruction_set)
    }
}
