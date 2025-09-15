use std::ops::ControlFlow;

use crate::{
    DebugError, DebugRegisters, StackFrame, exception_handling::ExceptionInterface,
    unwind_pc_without_debuginfo,
};

use probe_rs::{MemoryInterface, RegisterRole, RegisterValue};

pub struct XtensaExceptionHandler;

impl XtensaExceptionHandler {
    fn unwind_registers(
        &self,
        memory: &mut dyn MemoryInterface,
        unwind_registers: &mut DebugRegisters,
    ) -> Result<(), DebugError> {
        // WindowUnderflow12:
        // // On entry here: a0-a11 are call[i].reg[0..11] and initially contain garbage, a12-a15 are call[i+1].reg[0..3],
        // // (in particular, a13 is call[i+1]’s stack pointer) and must be preserved
        // l32e a0, a13, -16  // restore a0 from call[i+1]’s frame
        // l32e a1, a13, -12  // restore a1 from call[i+1]’s frame
        // l32e a2, a13, -8   // restore a2 from call[i+1]’s frame
        // l32e a11, a1, -12  // a11 <- call[i-1]’s sp
        // l32e a3, a13, -4   // restore a3 from call[i+1]’s frame
        // l32e a4, a11, -48  // restore a4 from end of call[i]’s frame
        // l32e a5, a11, -44  // restore a5 from end of call[i]’s frame
        // l32e a6, a11, -40  // restore a6 from end of call[i]’s frame
        // l32e a7, a11, -36  // restore a7 from end of call[i]’s frame
        // l32e a8, a11, -32  // restore a8 from end of call[i]’s frame
        // l32e a9, a11, -28  // restore a9 from end of call[i]’s frame
        // l32e a10, a11, -24 // restore a10 from end of call[i]’s frame
        // l32e a11, a11, -20 // restore a11 from end of call[i]’s frame
        // rfwu

        // We can try and use FP to unwind SP and RA that allows us to continue unwinding.

        let ra = unwind_registers.get_register_value_by_role(&RegisterRole::ReturnAddress)?;

        if ra == 0 {
            return Ok(());
        }

        // Current register values.
        let sp = unwind_registers.get_register_value_by_role(&RegisterRole::StackPointer)?;

        if sp < 16 {
            // Stack pointer is too low.
            return Err(DebugError::Other(
                "Stack pointer is too low to unwind".to_string(),
            ));
        }

        let windowsize = (ra & 0xc000_0000) >> 30;

        // Read A0-A3 from current stack frame's Register-Spill Area.
        let mut stack_frame = [0; 4];
        memory.read_32(sp - 16, &mut stack_frame)?;

        let [a0, caller_sp, a2, a3] = stack_frame;

        // TODO: use an architecture-appropriate value?
        if (caller_sp as u64).saturating_sub(sp) > 0x1000_0000 {
            // Stack pointer is too far away from the current stack pointer.
            return Err(DebugError::Other(
                "Stack pointer is too far away to unwind".to_string(),
            ));
        }

        let regs_from_current_frame = [
            (RegisterRole::ReturnAddress, a0),
            (RegisterRole::StackPointer, caller_sp),
            (RegisterRole::Core("a2"), a2),
            (RegisterRole::Core("a3"), a3),
        ];

        for (role, value) in regs_from_current_frame {
            let reg = unwind_registers.get_register_mut_by_role(&role).unwrap();
            reg.value = Some(RegisterValue::from(value));
        }

        if windowsize > 1 {
            // The rest of the registers are in the previous stack frame.
            let frame_sp = memory.read_word_32(caller_sp as u64 - 12)?;

            // We've already read 4 registers out of windowsize * 4.
            const AREGS: [&str; 8] = ["a4", "a5", "a6", "a7", "a8", "a9", "a10", "a11"];
            let mut frame = [0; AREGS.len()];

            let regs_to_read = windowsize * 4 - 4;
            let frame_to_read = &mut frame[..regs_to_read as usize];

            // For windowsize = 3(12 registers), the offset is -48
            memory.read_32(
                frame_sp as u64 - 16 - 4 * regs_to_read,
                &mut frame_to_read[..],
            )?;

            for (reg, reg_value) in AREGS.iter().zip(frame_to_read.iter().copied()) {
                let reg = unwind_registers
                    .get_register_mut_by_role(&RegisterRole::Core(reg))
                    .unwrap();
                reg.value = Some(RegisterValue::from(reg_value));
            }
        }

        Ok(())
    }
}

impl ExceptionInterface for XtensaExceptionHandler {
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
