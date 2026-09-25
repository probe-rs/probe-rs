use std::ops::ControlFlow;

use crate::{
    DebugError, DebugInfo, DebugRegisters, StackFrame,
    exception_handling::{ExceptionInfo, ExceptionInterface},
    get_object_reference, unwind_pc_without_debuginfo,
};

use probe_rs::{
    InstructionSet, MemoryInterface, RegisterRole, RegisterValue,
    architecture::riscv::registers::DWARF_CSR_BASE,
};

const MEPC_DWARF_ID: u16 = DWARF_CSR_BASE + 0x341;
const MCAUSE_DWARF_ID: u16 = DWARF_CSR_BASE + 0x342;

/// The registers that the `riscv-rt` trap stub pushes, in the order they appear on the stack,
/// identified by DWARF number. The DWARF number of a RISC-V general purpose register is its
/// `x` number.
const TRAP_FRAME_REGISTERS: &[u16] = &[
    1, // ra
    5, 6, 7, 28, 29, 30, 31, // t0-t6
    10, 11, 12, 13, 14, 15, 16, 17, // a0-a7
];

/// Whether `symbol` names one of the assembly stubs that sit between a trap and the Rust
/// handler. They carry no unwind information, so the interrupted frame has to be recovered from
/// `mepc` and the trap frame instead.
fn is_trap_entry(symbol: &str) -> bool {
    matches!(
        symbol,
        "_default_start_trap"
            | "_pre_default_start_trap"
            | "_pre_default_start_trap_ret"
            | "_continue_interrupt_trap"
    )
    // Vectored mode emits one stub per interrupt source, named `_start_<source>_trap`. The Rust
    // entry point `_start_trap_rust` does not match, and must not: it is a real frame.
    || (symbol.starts_with("_start_") && symbol.ends_with("_trap"))
}

/// Decodes `mcause` into a human readable description.
fn exception_description(mcause: u64) -> String {
    // Espressif cores pack the previous privilege level and interrupt level into the upper bits
    // of `mcause`, so the cause code cannot be recovered by masking off the interrupt bit alone.
    let code = mcause & 0xFFF;

    if mcause & (1 << 31) != 0 {
        let interrupt = match code {
            3 => "Machine software interrupt",
            7 => "Machine timer interrupt",
            11 => "Machine external interrupt",
            _ => return format!("Interrupt {code}"),
        };
        return interrupt.to_string();
    }

    let exception = match code {
        0 => "Instruction address misaligned",
        1 => "Instruction access fault",
        2 => "Illegal instruction",
        3 => "Breakpoint",
        4 => "Load address misaligned",
        5 => "Load access fault",
        6 => "Store/AMO address misaligned",
        7 => "Store/AMO access fault",
        8 => "Environment call from U-mode",
        9 => "Environment call from S-mode",
        11 => "Environment call from M-mode",
        12 => "Instruction page fault",
        13 => "Load page fault",
        15 => "Store/AMO page fault",
        _ => return format!("Exception {code}"),
    };
    format!("Exception <Cause: {exception}>")
}

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
    fn exception_details(
        &self,
        memory: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
        debug_info: &DebugInfo,
    ) -> Result<Option<ExceptionInfo>, DebugError> {
        // RISC-V has no architectural marker for "this frame returns from a trap": mcause, mstatus
        // and mepc all keep their values for the whole handler and beyond. The only reliable signal
        // is the trap stub itself.
        let frame_pc =
            stackframe_registers.get_register_value_by_role(&RegisterRole::ProgramCounter)?;
        if !debug_info
            .find_symbol(frame_pc)
            .is_some_and(|symbol| is_trap_entry(&symbol))
        {
            return Ok(None);
        }

        let address_size = stackframe_registers.get_address_size_bytes();
        if address_size != 4 {
            tracing::debug!(
                "UNWIND: Trap frame recovery is only implemented for 32-bit RISC-V cores."
            );
            return Ok(None);
        }

        let Some(trap_pc) = stackframe_registers
            .get_register_by_dwarf_id(MEPC_DWARF_ID)
            .and_then(|mepc| mepc.value)
        else {
            tracing::debug!("UNWIND: Reached a trap stub, but mepc was not captured.");
            return Ok(None);
        };
        if trap_pc.is_zero() {
            return Ok(None);
        }

        let mut registers = stackframe_registers.clone();
        let stack_pointer = registers.get_register_value_by_role(&RegisterRole::StackPointer)?;

        let mut trap_frame = vec![0u32; TRAP_FRAME_REGISTERS.len()];
        memory.read_32(stack_pointer, &mut trap_frame)?;

        for (&dwarf_id, &value) in TRAP_FRAME_REGISTERS.iter().zip(trap_frame.iter()) {
            let Some(register_id) = registers
                .get_register_by_dwarf_id(dwarf_id)
                .map(|register| register.core_register.id)
            else {
                continue;
            };
            // `unwrap`: the register was just found by the same key.
            #[expect(clippy::unwrap_used, reason = "the register was just looked up")]
            let register = registers.get_register_mut(register_id).unwrap();
            register.value = Some(RegisterValue::U32(value));
        }

        // The stub reserves the trap frame before it calls into Rust, and we only ever reach this
        // frame through that call, so the interrupted stack pointer is above the frame.
        let interrupted_stack_pointer =
            stack_pointer + (TRAP_FRAME_REGISTERS.len() * address_size) as u64;
        registers
            .get_register_mut_by_role(&RegisterRole::StackPointer)?
            .value = Some(registers.address_to_register_value(interrupted_stack_pointer));

        registers
            .get_register_mut_by_role(&RegisterRole::ProgramCounter)?
            .value = Some(trap_pc);

        let raw_exception = self.raw_exception(stackframe_registers)?;

        Ok(Some(ExceptionInfo {
            raw_exception,
            description: exception_description(raw_exception.into()),
            handler_frame: StackFrame {
                id: get_object_reference(),
                function_name: exception_description(raw_exception.into()),
                source_location: None,
                registers,
                pc: trap_pc,
                frame_base: None,
                is_inlined: false,
                local_variables: None,
                canonical_frame_address: None,
            },
        }))
    }

    fn raw_exception(&self, stackframe_registers: &DebugRegisters) -> Result<u32, DebugError> {
        let mcause = stackframe_registers
            .get_register_by_dwarf_id(MCAUSE_DWARF_ID)
            .and_then(|mcause| mcause.value)
            .ok_or_else(|| DebugError::Other("mcause was not captured".to_string()))?;

        let mcause: u64 = mcause.try_into()?;
        Ok(mcause as u32)
    }

    fn exception_description(
        &self,
        raw_exception: u32,
        _memory: &mut dyn MemoryInterface,
    ) -> Result<String, DebugError> {
        Ok(exception_description(raw_exception.into()))
    }

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

#[cfg(test)]
mod test {
    use super::{exception_description, is_trap_entry};

    #[test]
    fn trap_stubs_are_recognised() {
        for symbol in [
            "_default_start_trap",
            "_pre_default_start_trap",
            "_pre_default_start_trap_ret",
            "_continue_interrupt_trap",
            "_start_DefaultHandler_trap",
            "_start_Trap1_trap",
        ] {
            assert!(is_trap_entry(symbol), "{symbol} should be a trap stub");
        }
    }

    #[test]
    fn the_rust_trap_entry_is_a_real_frame() {
        // These have unwind information and must be unwound normally. Treating one as a stub would
        // insert a bogus frame into every trap backtrace.
        for symbol in [
            "_start_trap_rust",
            "_start_trap_rust_hal",
            "start_trap_rust_hal",
            "ExceptionHandler",
        ] {
            assert!(!is_trap_entry(symbol), "{symbol} should not be a trap stub");
        }
    }

    #[test]
    fn mcause_upper_bits_do_not_affect_the_cause() {
        // Espressif cores pack the previous privilege and interrupt level above the cause code.
        assert_eq!(
            exception_description(0x3800_0005),
            "Exception <Cause: Load access fault>"
        );
        assert_eq!(
            exception_description(0x8000_0007),
            "Machine timer interrupt"
        );
    }
}
