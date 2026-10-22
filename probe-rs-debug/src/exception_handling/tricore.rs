//! Unwinding for TriCore.
//!
//! `CALL` saves the upper context of the caller (`PCXI`, `PSW`, `A10`, `A11`, `D8`-`D15`,
//! `A12`-`A15`) into a context save area (CSA) in hardware and links it through the new `PCXI`.
//! The callee's `A11` is the return address into the caller. Compilers emit no unwind rules for
//! the saved registers, so they reach the unwinder as `Undefined`: they are read from the CSA
//! that the called frame's `PCXI` links to.

use std::{cell::RefCell, ops::ControlFlow};

use probe_rs::{
    CoreRegister, InstructionSet, MemoryInterface, RegisterRole, RegisterValue, UnwindRule,
    architecture::tricore::registers::{
        DWARF_FCX, DWARF_PCXI, csa_address, pcxi_links_upper_context, upper_context_word,
    },
};

use crate::{
    DebugError, DebugRegisters, StackFrame,
    exception_handling::{ExceptionInterface, ReturnAddressRecovery},
    unwind_pc_without_debuginfo,
};

/// The number of words of a context save area.
const CSA_WORDS: usize = 16;

/// The upper context of one calling frame.
struct CachedContext {
    pcxi: u32,
    words: [u32; CSA_WORDS],
}

/// The TriCore [`ExceptionInterface`].
#[derive(Default)]
pub struct TriCoreExceptionHandler {
    cache: RefCell<Option<CachedContext>>,
}

impl TriCoreExceptionHandler {
    /// Reads the upper context that the `PCXI` of `callee_frame_registers` links to, or `None`
    /// when there is no calling context.
    fn upper_context(
        &self,
        memory: &mut dyn MemoryInterface,
        callee_frame_registers: &DebugRegisters,
    ) -> Result<Option<[u32; CSA_WORDS]>, DebugError> {
        let Some(pcxi) = register_u32(callee_frame_registers, DWARF_PCXI) else {
            return Ok(None);
        };
        // PCXI = 0: the outermost frame (after reset, or a startup that clears it).
        if pcxi == 0 {
            return Ok(None);
        }
        if !pcxi_links_upper_context(pcxi) {
            // A lower context (saved by SVLCX or an interrupt handler) has another layout.
            tracing::debug!("UNWIND: PCXI {pcxi:#010x} links a lower context, stopping.");
            return Ok(None);
        }

        let address = csa_address(pcxi);
        // The used CSA chain ends where the free list starts.
        if let Some(fcx) = register_u32(callee_frame_registers, DWARF_FCX)
            && fcx != 0
            && csa_address(fcx) == address
        {
            return Ok(None);
        }

        if let Some(cached) = self.cache.borrow().as_ref()
            && cached.pcxi == pcxi
        {
            return Ok(Some(cached.words));
        }

        let mut words = [0u32; CSA_WORDS];
        memory.read_32(address as u64, &mut words)?;
        *self.cache.borrow_mut() = Some(CachedContext { pcxi, words });
        Ok(Some(words))
    }
}

fn register_u32(registers: &DebugRegisters, dwarf_id: u16) -> Option<u32> {
    let value = registers.get_register_by_dwarf_id(dwarf_id)?.value?;
    TryInto::<u32>::try_into(value).ok()
}

impl ExceptionInterface for TriCoreExceptionHandler {
    fn return_address_recovery(&self) -> ReturnAddressRecovery {
        // The calling frame's `A11` comes from the CSA and is its own return address; the
        // return address into it is the called frame's `A11`.
        ReturnAddressRecovery::CalledFrameRegister
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

        if let Some(word) = debug_register.dwarf_id.and_then(upper_context_word) {
            *register_rule = "TriCore upper context CSA (dwarf Undefined)".to_string();
            return Ok(self
                .upper_context(memory, callee_frame_registers)?
                .map(|context| RegisterValue::U32(context[word])));
        }

        Ok(match debug_register.unwind_rule {
            UnwindRule::Preserve => {
                *register_rule = "Preserve (dwarf Undefined)".to_string();
                callee_frame_registers
                    .get_register(debug_register.id)
                    .and_then(|reg| reg.value)
            }
            UnwindRule::Clear | UnwindRule::SpecialRule => {
                *register_rule = "Clear (dwarf Undefined)".to_string();
                None
            }
        })
    }

    fn unwind_without_debuginfo(
        &self,
        unwind_registers: &mut DebugRegisters,
        frame_pc: u64,
        _stack_frames: &[StackFrame],
        instruction_set: Option<InstructionSet>,
        memory: &mut dyn MemoryInterface,
    ) -> ControlFlow<Option<DebugError>> {
        let callee_frame_registers = unwind_registers.clone();

        // The program counter of the caller comes from the called frame's A11.
        unwind_pc_without_debuginfo(unwind_registers, frame_pc, instruction_set)?;

        let context = match self.upper_context(memory, &callee_frame_registers) {
            Ok(Some(context)) => context,
            Ok(None) => return ControlFlow::Break(None),
            Err(error) => return ControlFlow::Break(Some(error)),
        };

        for register in unwind_registers.0.iter_mut() {
            if register
                .core_register
                .register_has_role(RegisterRole::ProgramCounter)
            {
                continue;
            }
            register.value = match register.core_register.dwarf_id.and_then(upper_context_word) {
                Some(word) => Some(RegisterValue::U32(context[word])),
                None if register.core_register.unwind_rule == UnwindRule::Preserve => {
                    register.value
                }
                None => None,
            };
        }

        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use probe_rs::{architecture::tricore::registers::TRICORE_CORE_REGISTERS, test::MockMemory};

    const CSA: u32 = 0x7000_9000;

    fn registers(pcxi: u32, a11: u32) -> DebugRegisters {
        let mut registers = DebugRegisters::from_core_registers(&TRICORE_CORE_REGISTERS, |_| {
            Some(RegisterValue::U32(0))
        });
        let set = |registers: &mut DebugRegisters, dwarf: u16, value: u32| {
            let id = registers
                .get_register_by_dwarf_id(dwarf)
                .unwrap()
                .core_register
                .id;
            registers.get_register_mut(id).unwrap().value = Some(RegisterValue::U32(value));
        };
        set(&mut registers, DWARF_PCXI, pcxi);
        set(&mut registers, 27, a11);
        set(&mut registers, 42, 0x8021_C004);
        set(&mut registers, 16, 0xA0A0_A0A0); // A0 is global
        set(&mut registers, 4, 0x4444); // D4 is caller saved
        registers
    }

    fn memory() -> MockMemory {
        let mut memory = MockMemory::new();
        // PCXI, PSW, A10, A11, D8-D11, A12-A15, D12-D15
        let context: [u32; 16] = [
            0,
            0x0B80,
            0x7000_BFF0,
            0x8021_C0B0,
            8,
            9,
            10,
            11,
            12,
            13,
            0x7000_BFF8,
            15,
            0xC,
            0xD,
            0xE,
            0xF,
        ];
        memory.add_word_range(CSA as u64, &context);
        memory
    }

    // A PCXI with UL set, linking the CSA at 0x7000_9000.
    const PCXI: u32 = (1 << 20) | 0x0007_0240;

    #[test]
    fn upper_context_registers_come_from_the_csa() {
        let handler = TriCoreExceptionHandler::default();
        let callee = registers(PCXI, 0x8021_C0A2);
        let mut memory = memory();
        let mut rule = String::new();

        let value = |dwarf: u16, memory: &mut MockMemory, rule: &mut String| {
            let register = callee
                .get_register_by_dwarf_id(dwarf)
                .unwrap()
                .core_register;
            handler
                .unwind_undefined_register(register, &callee, None, memory, rule)
                .unwrap()
        };

        assert_eq!(
            value(26, &mut memory, &mut rule),
            Some(RegisterValue::U32(0x7000_BFF0))
        );
        assert_eq!(
            value(27, &mut memory, &mut rule),
            Some(RegisterValue::U32(0x8021_C0B0))
        );
        assert_eq!(
            value(30, &mut memory, &mut rule),
            Some(RegisterValue::U32(0x7000_BFF8))
        );
        assert_eq!(
            value(DWARF_PCXI, &mut memory, &mut rule),
            Some(RegisterValue::U32(0))
        );
        assert_eq!(
            value(12, &mut memory, &mut rule),
            Some(RegisterValue::U32(0xC))
        );
        // Global registers keep their value, caller-saved registers are unknown.
        assert_eq!(
            value(16, &mut memory, &mut rule),
            Some(RegisterValue::U32(0xA0A0_A0A0))
        );
        assert_eq!(value(4, &mut memory, &mut rule), None);
    }

    #[test]
    fn the_outermost_frame_has_no_context() {
        let handler = TriCoreExceptionHandler::default();
        let callee = registers(0, 0);
        let mut memory = memory();
        let register = callee.get_register_by_dwarf_id(26).unwrap().core_register;
        let value = handler
            .unwind_undefined_register(register, &callee, None, &mut memory, &mut String::new())
            .unwrap();
        assert_eq!(value, None);
    }

    #[test]
    fn unwind_without_debuginfo_follows_the_csa_chain() {
        let handler = TriCoreExceptionHandler::default();
        let mut registers = registers(PCXI, 0x8021_C0A2);
        let mut memory = memory();

        let flow = handler.unwind_without_debuginfo(
            &mut registers,
            0x8021_C004,
            &[],
            Some(InstructionSet::TriCore),
            &mut memory,
        );
        assert!(flow.is_continue());
        // The caller PC points into the CALL instruction before the return address.
        assert_eq!(
            registers
                .get_register_value_by_role(&RegisterRole::ProgramCounter)
                .unwrap(),
            0x8021_C0A0
        );
        assert_eq!(
            registers
                .get_register_value_by_role(&RegisterRole::ReturnAddress)
                .unwrap(),
            0x8021_C0B0
        );
        assert_eq!(
            registers
                .get_register_value_by_role(&RegisterRole::StackPointer)
                .unwrap(),
            0x7000_BFF0
        );
    }
}
