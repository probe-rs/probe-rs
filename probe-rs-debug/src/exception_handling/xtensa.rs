use std::{cell::RefCell, ops::ControlFlow};

use crate::{
    DebugError, DebugInfo, DebugRegisters, StackFrame,
    exception_handling::{ExceptionInfo, ExceptionInterface, ReturnAddressRecovery},
    get_object_reference, unwind_pc_without_debuginfo,
};

use probe_rs::{
    CoreRegister, InstructionSet, MemoryInterface, RegisterRole, RegisterValue, UnwindRule,
};

/// Word offsets into the block that [`XtensaExceptionHandler::read_context_frame`] reads. It
/// starts at the register spill slots below the context frame that the `xtensa-lx-rt` exception
/// stubs build on the stack of the interrupted code.
///
/// The stub sets these offsets unconditionally, and `xtensa_lx_rt::exception::Context` mirrors
/// them, so a core that saves more state appends to the frame and leaves them alone. The length
/// of the frame is not part of the layout: the stub picks it to fit the registers it saves,
/// rounded up so that it can reach the frame with a single `addmi`.
mod context {
    /// The stub copies the interrupted program counter and stack pointer into the spill slots at
    /// -16 and -12, so that a backtrace that knows nothing about exceptions walks through one.
    pub const SPILL_PC: usize = 0;
    pub const SPILL_A1: usize = 1;

    /// The spill slots occupy the four words below the frame.
    const FRAME: usize = 4;

    pub const PC: usize = FRAME;
    pub const PS: usize = FRAME + 1;
    /// `A0` to `A15` follow each other, in DWARF register order.
    pub const A0: usize = FRAME + 2;
    pub const A1: usize = A0 + 1;
    pub const EXCCAUSE: usize = FRAME + 19;

    /// The number of words to read. The frame is longer, but the rest holds coprocessor state
    /// that the unwind does not need.
    pub const WORDS: usize = FRAME + 20;

    /// How much of the frame the offsets above cover.
    pub const READ_SIZE: u64 = ((WORDS - FRAME) * 4) as u64;
}

/// `EXCCAUSE` of a level 1 interrupt, which shares its vector with the general exceptions.
const LEVEL_ONE_INTERRUPT: u32 = 4;
/// `xtensa_lx_rt::exception::ExceptionCause::None`, reported where no cause was saved.
const NO_CAUSE: u32 = 255;

/// The kind of exception that an `xtensa-lx-rt` assembly stub handles.
enum ExceptionEntry {
    /// A general exception or a level 1 interrupt. The stub saves the cause.
    Exception,
    /// A double exception. The stub saves the cause.
    DoubleException,
    /// A high level interrupt. The stub saves no cause.
    Interrupt(u8),
}

impl ExceptionEntry {
    /// The stub that `symbol` names, if it is one of the assembly stubs that sit between a vector
    /// and the Rust handler. They carry no unwind information, so the interrupted frame has to be
    /// recovered from the context frame instead.
    ///
    /// The Rust entry points (`__user_exception`, `__level_1_interrupt`, ...) must not match: they
    /// are real frames.
    fn of_symbol(symbol: &str) -> Option<Self> {
        let stub = symbol
            .strip_prefix("__naked_")
            .or_else(|| symbol.strip_prefix("__default_naked_"))?;

        Some(match stub {
            "exception" | "user_exception" | "kernel_exception" => Self::Exception,
            "double_exception" => Self::DoubleException,
            _ => {
                let level = stub.strip_prefix("level_")?.strip_suffix("_interrupt")?;
                Self::Interrupt(level.parse().ok()?)
            }
        })
    }

    /// Whether the stub saves `EXCCAUSE`. Only the level 1 vector runs while the register still
    /// describes the exception, so a high level interrupt leaves the field untouched.
    fn saves_cause(&self) -> bool {
        !matches!(self, Self::Interrupt(_))
    }

    fn description(&self, exccause: u32) -> String {
        match self {
            Self::Exception if exccause == LEVEL_ONE_INTERRUPT => "Level 1 interrupt".to_string(),
            Self::Exception => format!("Exception <Cause: {}>", exception_cause(exccause)),
            Self::DoubleException => {
                format!("Double exception <Cause: {}>", exception_cause(exccause))
            }
            Self::Interrupt(level) => format!("Level {level} interrupt"),
        }
    }
}

/// Decodes an `EXCCAUSE` value into a human readable description.
fn exception_cause(exccause: u32) -> String {
    let cause = match exccause {
        0 => "Illegal instruction",
        1 => "System call",
        2 => "Instruction fetch error",
        3 => "Load/store error",
        4 => "Level 1 interrupt",
        5 => "Alloca",
        6 => "Integer divide by zero",
        8 => "Privileged instruction",
        9 => "Unaligned load or store",
        10 => "External register privilege error",
        11 => "Exclusive error",
        12 => "Instruction fetch data error",
        13 => "Load/store data error",
        14 => "Instruction fetch address error",
        15 => "Load/store address error",
        16 => "ITLB miss",
        17 => "ITLB multihit",
        18 => "Instruction fetch privilege",
        20 => "Instruction fetch prohibited",
        24 => "DTLB miss",
        25 => "DTLB multihit",
        26 => "Load/store privilege",
        28 => "Load prohibited",
        29 => "Store prohibited",
        32..=39 => return format!("Coprocessor {} disabled", exccause - 32),
        NO_CAUSE => "Not saved by the handler",
        _ => return format!("Cause {exccause}"),
    };
    cause.to_string()
}

#[derive(Default)]
pub struct XtensaExceptionHandler {
    /// Single-entry cache of the most recent windowed-ABI unwind, keyed by the callee's stack
    /// pointer. Avoids re-reading the spill area for each register when
    /// [`Self::unwind_undefined_register`] is invoked repeatedly for the same frame.
    cache: RefCell<Option<UnwindCache>>,
}

struct UnwindCache {
    sp: u64,
    unwound: DebugRegisters,
}

impl XtensaExceptionHandler {
    /// Populate [`Self::cache`] with the windowed-ABI unwind result for `callee_frame_registers`,
    /// reusing the previous result if the callee SP matches.
    fn ensure_cached_unwind(
        &self,
        memory: &mut dyn MemoryInterface,
        callee_frame_registers: &DebugRegisters,
    ) -> Result<(), DebugError> {
        let sp = callee_frame_registers.get_register_value_by_role(&RegisterRole::StackPointer)?;

        if matches!(self.cache.borrow().as_ref(), Some(c) if c.sp == sp) {
            return Ok(());
        }

        let mut scratch = callee_frame_registers.clone();
        self.unwind_registers(memory, &mut scratch)?;
        *self.cache.borrow_mut() = Some(UnwindCache {
            sp,
            unwound: scratch,
        });

        Ok(())
    }

    /// Read the context frame that an exception stub built at `context_frame`, or `None` when the
    /// memory there is not one.
    ///
    /// Landing in a stub says that an exception frame should be here, but not that the stack
    /// pointer found it. A window unwind that went wrong earlier, or a halt before the stub
    /// finished building the frame, both lead here with an address that holds something else.
    /// Reading it anyway would turn a wrong stack pointer into a confident, wrong frame.
    fn read_context_frame(
        &self,
        memory: &mut dyn MemoryInterface,
        context_frame: u64,
    ) -> Result<Option<[u32; context::WORDS]>, DebugError> {
        let Some(spill_area) = context_frame.checked_sub(16) else {
            return Ok(None);
        };

        let mut context = [0u32; context::WORDS];
        memory.read_32(spill_area, &mut context)?;

        // The stub writes the interrupted program counter and stack pointer both into the frame
        // and into the spill slots below it. A frame that does not agree with itself is not one.
        // The saved stack pointer also bounds the frame from above, because the stub reached the
        // frame by lowering it.
        let interrupted_sp = context[context::A1];
        if context[context::SPILL_PC] != context[context::PC]
            || context[context::SPILL_A1] != interrupted_sp
            || context[context::PC] == 0
            || (interrupted_sp as u64) < context_frame + context::READ_SIZE
        {
            tracing::debug!(
                "UNWIND: Reached an exception stub, but {context_frame:#010x} does not hold a context frame."
            );
            return Ok(None);
        }

        Ok(Some(context))
    }

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

        if caller_sp as u64 <= sp {
            // The stack grows down, so the stack pointer of the caller must be above.
            return Err(DebugError::Other(
                "Stack pointer of the caller is not above the current stack pointer".to_string(),
            ));
        }

        // TODO: use an architecture-appropriate value?
        if caller_sp as u64 - sp > 0x1000_0000 {
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
            let Some(caller_sp) = caller_sp.checked_sub(12) else {
                return Ok(());
            };
            let frame_sp = memory.read_word_32(caller_sp as u64)?;

            // We've already read 4 registers out of windowsize * 4.
            const AREGS: [&str; 8] = ["a4", "a5", "a6", "a7", "a8", "a9", "a10", "a11"];
            let mut frame = [0; AREGS.len()];

            let regs_to_read = windowsize * 4 - 4;
            let frame_to_read = &mut frame[..regs_to_read as usize];

            // For windowsize = 3(12 registers), the offset is -48
            let sp_offset = 16 + 4 * regs_to_read;

            let Some(frame_sp) = (frame_sp as u64).checked_sub(sp_offset) else {
                return Ok(());
            };

            memory.read_32(frame_sp, &mut frame_to_read[..])?;

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
    fn exception_details(
        &self,
        memory: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
        debug_info: &DebugInfo,
    ) -> Result<Option<ExceptionInfo>, DebugError> {
        // Xtensa has no architectural marker for "this frame returns from an exception": EPC and
        // EXCCAUSE keep their values for the whole handler and beyond. The only reliable signal
        // is the stub itself.
        let frame_pc =
            stackframe_registers.get_register_value_by_role(&RegisterRole::ProgramCounter)?;
        let Some(entry) = debug_info
            .find_symbol(frame_pc)
            .as_deref()
            .and_then(ExceptionEntry::of_symbol)
        else {
            return Ok(None);
        };

        // The stub reserves the context frame before it calls into Rust, and we only ever reach
        // this frame through that call, so the stack pointer is the base of the frame.
        let context_frame =
            stackframe_registers.get_register_value_by_role(&RegisterRole::StackPointer)?;

        let Some(context) = self.read_context_frame(memory, context_frame)? else {
            return Ok(None);
        };

        let interrupted_pc = context[context::PC];
        let mut registers = stackframe_registers.clone();

        // The context frame holds `a0` to `a15` in DWARF register order, so `a1` restores the
        // interrupted stack pointer along with the rest.
        for (dwarf_id, &value) in context[context::A0..][..16].iter().enumerate() {
            let Some(register_id) = registers
                .get_register_by_dwarf_id(dwarf_id as u16)
                .map(|register| register.core_register.id)
            else {
                continue;
            };
            // `unwrap`: the register was just found by the same key.
            #[expect(clippy::unwrap_used, reason = "the register was just looked up")]
            let register = registers.get_register_mut(register_id).unwrap();
            register.value = Some(RegisterValue::U32(value));
        }

        if let Ok(ps) = registers.get_register_mut_by_role(&RegisterRole::ProcessorStatus) {
            ps.value = Some(RegisterValue::U32(context[context::PS]));
        }

        let interrupted_pc = RegisterValue::U32(interrupted_pc);
        registers
            .get_register_mut_by_role(&RegisterRole::ProgramCounter)?
            .value = Some(interrupted_pc);

        let raw_exception = if entry.saves_cause() {
            context[context::EXCCAUSE]
        } else {
            NO_CAUSE
        };
        let description = entry.description(raw_exception);

        Ok(Some(ExceptionInfo {
            raw_exception,
            description: description.clone(),
            handler_frame: StackFrame {
                id: get_object_reference(),
                function_name: description,
                source_location: None,
                registers,
                pc: interrupted_pc,
                frame_base: None,
                is_inlined: false,
                local_variables: None,
                canonical_frame_address: None,
            },
        }))
    }

    fn exception_description(
        &self,
        raw_exception: u32,
        _memory: &mut dyn MemoryInterface,
    ) -> Result<String, DebugError> {
        Ok(exception_cause(raw_exception))
    }

    fn return_address_recovery(&self) -> ReturnAddressRecovery {
        // [`Self::unwind_undefined_register`] recovers `a0` from the register window, which is
        // the return address of the calling frame, not the return address into it.
        ReturnAddressRecovery::CalledFrameRegister
    }

    fn unwind_without_debuginfo(
        &self,
        unwind_registers: &mut DebugRegisters,
        frame_pc: u64,
        _stack_frames: &[StackFrame],
        instruction_set: Option<InstructionSet>,
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

    fn unwind_undefined_register(
        &self,
        debug_register: &CoreRegister,
        callee_frame_registers: &DebugRegisters,
        _unwind_cfa: Option<u64>,
        memory: &mut dyn MemoryInterface,
        register_rule: &mut String,
    ) -> Result<Option<RegisterValue>, DebugError> {
        if debug_register.register_has_role(RegisterRole::ProgramCounter) {
            unreachable!("The program counter is handled separately")
        }

        if self
            .ensure_cached_unwind(memory, callee_frame_registers)
            .is_ok()
        {
            let cache = self.cache.borrow();
            let new = cache
                .as_ref()
                .and_then(|c| c.unwound.get_register(debug_register.id))
                .and_then(|r| r.value);
            let old = callee_frame_registers
                .get_register(debug_register.id)
                .and_then(|r| r.value);

            if new != old {
                *register_rule = "Xtensa window spill (dwarf Undefined)".to_string();
                return Ok(new);
            }
        }

        Ok(match debug_register.unwind_rule {
            UnwindRule::Preserve => {
                *register_rule = "Preserve (dwarf Undefined)".to_string();
                callee_frame_registers
                    .get_register(debug_register.id)
                    .and_then(|reg| reg.value)
            }
            UnwindRule::Clear => {
                *register_rule = "Clear (dwarf Undefined)".to_string();
                None
            }
            UnwindRule::SpecialRule => {
                *register_rule = "Clear (no unwind rules specified)".to_string();
                None
            }
        })
    }
}

#[cfg(test)]
mod test {
    use std::path::PathBuf;

    use probe_rs::{
        MemoryInterface, RegisterRole, RegisterValue,
        architecture::xtensa::registers::XTENSA_CORE_REGISTERS, test::MockMemory,
    };

    use super::{ExceptionEntry, XtensaExceptionHandler, context};
    use crate::{DebugInfo, DebugRegisters, exception_handling::ExceptionInterface};

    #[test]
    fn exception_stubs_are_recognised() {
        for symbol in [
            "__naked_user_exception",
            "__naked_kernel_exception",
            "__naked_double_exception",
            "__default_naked_exception",
            "__default_naked_double_exception",
            "__naked_level_2_interrupt",
            "__default_naked_level_7_interrupt",
        ] {
            assert!(
                ExceptionEntry::of_symbol(symbol).is_some(),
                "{symbol} should be an exception stub"
            );
        }
    }

    #[test]
    fn the_rust_handlers_are_real_frames() {
        // These have unwind information and must be unwound normally. Treating one as a stub
        // would insert a bogus frame into every exception backtrace.
        for symbol in [
            "__exception",
            "__user_exception",
            "__default_exception",
            "__default_user_exception",
            "__level_1_interrupt",
            "__default_interrupt",
            "_UserExceptionVector",
        ] {
            assert!(
                ExceptionEntry::of_symbol(symbol).is_none(),
                "{symbol} should not be an exception stub"
            );
        }
    }

    /// Past the entry of `__naked_user_exception` in `async_esp32s3.elf`, where the stub has
    /// called the Rust handler.
    const STUB_PC: u32 = 0x4037_ae20;
    const CONTEXT_FRAME: u32 = 0x3fca_0000;
    const INTERRUPTED_PC: u32 = 0x4200_1234;
    /// `XT_STK_FRMSZ` at the time of writing. The recovery must not depend on the value.
    const INTERRUPTED_SP: u32 = CONTEXT_FRAME + 256;

    /// The words that an exception stub leaves from the spill slots below the frame upwards.
    fn context_frame() -> [u32; context::WORDS] {
        let mut frame = [0u32; context::WORDS];
        frame[context::SPILL_PC] = INTERRUPTED_PC;
        frame[context::SPILL_A1] = INTERRUPTED_SP;
        frame[context::PC] = INTERRUPTED_PC;
        frame[context::PS] = 0x0002_0021;
        frame[context::A0] = 0x8200_5678;
        frame[context::A1] = INTERRUPTED_SP;
        frame[context::EXCCAUSE] = 28;
        frame
    }

    fn stub_registers() -> DebugRegisters {
        let mut registers = DebugRegisters::from_core_registers(&XTENSA_CORE_REGISTERS, |_| None);
        registers
            .get_register_mut_by_role(&RegisterRole::ProgramCounter)
            .unwrap()
            .value = Some(RegisterValue::U32(STUB_PC));
        registers
            .get_register_mut_by_role(&RegisterRole::StackPointer)
            .unwrap()
            .value = Some(RegisterValue::U32(CONTEXT_FRAME));
        registers
    }

    fn debug_info() -> DebugInfo {
        let elf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/debug-unwind-tests/async_esp32s3.elf");
        DebugInfo::from_file(elf).unwrap()
    }

    #[test]
    fn the_interrupted_frame_comes_from_the_context_frame() {
        let mut memory = MockMemory::new();
        memory.add_word_range(CONTEXT_FRAME as u64 - 16, &context_frame());

        let exception = XtensaExceptionHandler::default()
            .exception_details(
                &mut memory as &mut dyn MemoryInterface,
                &stub_registers(),
                &debug_info(),
            )
            .unwrap()
            .expect("the stub should be recognised as an exception entry");

        assert_eq!(exception.description, "Exception <Cause: Load prohibited>");
        assert_eq!(
            exception.handler_frame.pc,
            RegisterValue::U32(INTERRUPTED_PC)
        );

        let interrupted = &exception.handler_frame.registers;
        assert_eq!(
            interrupted
                .get_register_value_by_role(&RegisterRole::StackPointer)
                .unwrap(),
            INTERRUPTED_SP as u64
        );
        assert_eq!(
            interrupted
                .get_register_value_by_role(&RegisterRole::ReturnAddress)
                .unwrap(),
            0x8200_5678
        );
    }

    /// A stack pointer that does not point at a context frame must not produce one. Otherwise a
    /// window unwind that went wrong earlier ends in a frame that looks authoritative.
    #[test]
    fn a_frame_that_does_not_agree_with_itself_is_rejected() {
        for (name, break_it) in [
            (
                "frame length",
                (|f: &mut [u32; context::WORDS]| {
                    f[context::A1] = CONTEXT_FRAME + 16;
                    f[context::SPILL_A1] = CONTEXT_FRAME + 16;
                }) as fn(&mut [u32; context::WORDS]),
            ),
            ("spilled pc", |f| f[context::SPILL_PC] = 0xdead_beef),
            ("spilled sp", |f| f[context::SPILL_A1] = 0xdead_beef),
            ("pc", |f| {
                f[context::PC] = 0;
                f[context::SPILL_PC] = 0;
            }),
        ] {
            let mut frame = context_frame();
            break_it(&mut frame);

            let mut memory = MockMemory::new();
            memory.add_word_range(CONTEXT_FRAME as u64 - 16, &frame);

            let exception = XtensaExceptionHandler::default()
                .exception_details(
                    &mut memory as &mut dyn MemoryInterface,
                    &stub_registers(),
                    &debug_info(),
                )
                .unwrap();

            assert!(
                exception.is_none(),
                "a frame with a broken {name} should be rejected"
            );
        }
    }
}
