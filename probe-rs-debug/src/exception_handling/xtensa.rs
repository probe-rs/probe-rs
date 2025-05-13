use std::ops::ControlFlow;

use crate::{
    DebugError, DebugRegisters, StackFrame, exception_handling::ExceptionInterface,
    unwind_pc_without_debuginfo,
};

use probe_rs::{MemoryInterface, RegisterRole, RegisterValue};

pub struct XtensaExceptionHandler;

#[async_trait::async_trait(?Send)]
impl ExceptionInterface for XtensaExceptionHandler {
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

        // Current register values.
        let sp = unwind_registers
            .get_register_value_by_role(&RegisterRole::StackPointer)
            .unwrap();

        if sp < 16 {
            // Stack pointer is too low.
            return ControlFlow::Break(None);
        }

        let ra = unwind_registers
            .get_register_value_by_role(&RegisterRole::ReturnAddress)
            .unwrap();
        let windowsize = (ra & 0xc000_0000) >> 30;

        // Read A0-A3 from current stack frame's Register-Spill Area.
        let mut stack_frame = [0; 4];
        if let Err(e) = memory.read_32(sp - 16, &mut stack_frame).await {
            // FP points at something we can't read.
            return ControlFlow::Break(Some(e.into()));
        }

        let [a0, caller_sp, a2, a3] = stack_frame;

        if (caller_sp as u64).saturating_sub(sp) > 0x1000_0000 {
            // Stack pointer is too far away from the current stack pointer.
            return ControlFlow::Break(None);
        }

        let unwound_ra = unwind_registers
            .get_register_mut_by_role(&RegisterRole::ReturnAddress)
            .unwrap();
        unwound_ra.value = Some(RegisterValue::from(a0));

        let unwound_sp = unwind_registers
            .get_register_mut_by_role(&RegisterRole::StackPointer)
            .unwrap();
        unwound_sp.value = Some(RegisterValue::from(caller_sp));

        let unwound_a2 = unwind_registers
            .get_register_mut_by_role(&RegisterRole::Core("a2"))
            .unwrap();
        unwound_a2.value = Some(RegisterValue::from(a2));

        let unwound_a3 = unwind_registers
            .get_register_mut_by_role(&RegisterRole::Core("a3"))
            .unwrap();
        unwound_a3.value = Some(RegisterValue::from(a3));

        if windowsize > 1 {
            // The rest of the registers are in the previous stack frame.
            let frame_sp = match memory.read_word_32(caller_sp as u64 - 12).await {
                Ok(sp) => sp,
                Err(e) => {
                    // FP points at something we can't read.
                    return ControlFlow::Break(Some(e.into()));
                }
            };

            // We've already read 4 registers out of windowsize * 4.
            const AREGS: [&str; 8] = ["a4", "a5", "a6", "a7", "a8", "a9", "a10", "a11"];
            let mut frame = [0; AREGS.len()];

            let regs_to_read = windowsize * 4 - 4;
            let frame_to_read = &mut frame[..regs_to_read as usize];

            // For windowsize = 3(12 registers), the offset is -48
            if let Err(e) = memory
                .read_32(
                    frame_sp as u64 - 16 - 4 * regs_to_read,
                    &mut frame_to_read[..],
                )
                .await
            {
                // FP points at something we can't read.
                return ControlFlow::Break(Some(e.into()));
            }

            for (reg, reg_value) in AREGS.iter().zip(frame_to_read.iter().copied()) {
                let reg = unwind_registers
                    .get_register_mut_by_role(&RegisterRole::Core(reg))
                    .unwrap();
                reg.value = Some(RegisterValue::from(reg_value));
            }
        }

        if sp == caller_sp as u64 {
            return ControlFlow::Break(None);
        }

        ControlFlow::Continue(())
    }
}
