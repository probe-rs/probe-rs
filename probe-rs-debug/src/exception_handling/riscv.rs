use std::ops::ControlFlow;

use crate::{
    DebugError, DebugRegisters, StackFrame, exception_handling::ExceptionInterface,
    unwind_pc_without_debuginfo,
};

use probe_rs::{MemoryInterface, RegisterRole, RegisterValue};

pub struct RiscvExceptionHandler;

impl RiscvExceptionHandler {
    fn unwind_registers(
        &self,
        memory: &mut dyn MemoryInterface,
        unwind_registers: &mut DebugRegisters,
    ) -> Result<(), DebugError> {
        // Current register values.
        let sp = unwind_registers.get_register_value_by_role(&RegisterRole::StackPointer)?;

        if sp < 8 {
            // Stack pointer is too low, cannot unwind.
            return Err(DebugError::Other(format!(
                "Stack pointer {sp:#010x} is too low to unwind",
            )));
        }

        let mut stack_frame = [0; 2];
        memory.read_32(sp - 8, &mut stack_frame)?;

        let [caller_sp, return_addr] = stack_frame;

        // TODO: use an architecture-appropriate value?
        if (caller_sp as u64).saturating_sub(sp) > 0x1000_0000 {
            // Stack pointer is too far away from the current stack pointer.
            return Err(DebugError::Other(
                "Stack pointer is too far away to unwind".to_string(),
            ));
        }

        // TODO: unwind other registers as well.
        let regs_from_current_frame = [
            (RegisterRole::ReturnAddress, return_addr),
            (RegisterRole::StackPointer, caller_sp),
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
        instruction_set: Option<probe_rs::InstructionSet>,
        memory: &mut dyn MemoryInterface,
    ) -> ControlFlow<Option<DebugError>> {
        // Use the default method to unwind PC.
        unwind_pc_without_debuginfo(unwind_registers, frame_pc, instruction_set)?;

        // TODO: this should be automatically handled by the caller.
        match self.unwind_registers(memory, unwind_registers) {
            Ok(_) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(Some(error)),
        }
    }
}
