//! Minimal ARMv4T (ARM7TDMI) instruction simulator, used to calculate where the next
//! instruction executes, and in which instruction set, so that `Arm7tdmi::step` knows
//! where the core is after a single step.
//!
//! A watchpoint matching every instruction fetch does not reliably stop after exactly one
//! instruction on ARM7TDMI, since a RESTART executes a short burst of instructions first. Like
//! OpenOCD's `arm7_9_step` (via `arm_simulate_step`), the current instruction is decoded
//! instead to calculate the next PC, covering conditional execution, branches and any
//! instruction that writes PC. Only control flow is simulated: an instruction not recognised
//! here is assumed not to redirect it, and PC advances by the instruction size.
//!
//! Exceptions the instruction itself raises (SWI, undefined instructions, coprocessor
//! instructions on this coprocessor-less core) are predicted too. Exceptions that depend on
//! the system rather than the instruction (data/prefetch aborts) cannot be predicted here;
//! `step()` detects those afterwards from the core mode.
//!
//! Known limitation, not handled: `MSR` changing CPSR's T (Thumb) bit directly would change how
//! the *next* instruction is decoded without moving PC - architecturally unpredictable on
//! ARMv4T and not something real code does.

use super::communication_interface::{Arm7tdmiCommunicationInterface, Arm7tdmiError, CPSR_TBIT};

/// Exceptions an instruction can raise, with their (low) vector addresses and entry modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Exception {
    Undefined,
    SoftwareInterrupt,
    DataAbort,
    Irq,
    Fiq,
}

impl Exception {
    /// The exception vector address (ARM7TDMI has no high-vectors option).
    pub(super) fn vector(self) -> u32 {
        match self {
            Exception::Undefined => 0x04,
            Exception::SoftwareInterrupt => 0x08,
            Exception::DataAbort => 0x10,
            Exception::Irq => 0x18,
            Exception::Fiq => 0x1C,
        }
    }

    /// The processor mode the exception is taken in (CPSR\[4:0\]).
    pub(super) fn mode(self) -> u32 {
        match self {
            Exception::Undefined => 0x1B,
            Exception::SoftwareInterrupt => 0x13,
            Exception::DataAbort => 0x17,
            Exception::Irq => 0x12,
            Exception::Fiq => 0x11,
        }
    }

    /// The exception entered when the core switches into `mode` without the stepped instruction
    /// having asked for it. SVC mode can only be entered that way by a SWI; Abort mode is
    /// reported as a data abort (a prefetch abort of the stepped instruction itself would have
    /// happened before it ran, and looks the same from here).
    pub(super) fn from_mode(mode: u32) -> Option<Self> {
        match mode {
            0x1B => Some(Exception::Undefined),
            0x13 => Some(Exception::SoftwareInterrupt),
            0x17 => Some(Exception::DataAbort),
            0x12 => Some(Exception::Irq),
            0x11 => Some(Exception::Fiq),
            _ => None,
        }
    }
}

/// Where execution continues after the simulated instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NextInstruction {
    /// Address of the next instruction.
    pub pc: u32,
    /// Whether the next instruction executes in Thumb state.
    pub thumb: bool,
    /// Set when the instruction raises an exception (`pc` is then the vector).
    pub exception: Option<Exception>,
    /// Whether the instruction may change the processor mode on its own (MSR, or a PC write
    /// that restores CPSR from SPSR) - `step()` must not mistake that for an exception entry.
    pub changes_mode: bool,
}

impl NextInstruction {
    fn arm(pc: u32) -> Self {
        Self {
            pc: pc & !0b11,
            thumb: false,
            exception: None,
            changes_mode: false,
        }
    }

    fn thumb(pc: u32) -> Self {
        Self {
            pc: pc & !0b1,
            thumb: true,
            exception: None,
            changes_mode: false,
        }
    }

    /// A branch target whose instruction set is selected by bit 0 (BX).
    fn interworking(target: u32) -> Self {
        if target & 1 != 0 {
            Self::thumb(target)
        } else {
            Self::arm(target)
        }
    }

    /// A PC write that also copies SPSR into CPSR (`MOVS PC, ..`, `LDM {.., PC}^`), which
    /// takes the instruction set from SPSR's T bit.
    fn exception_return(target: u32, spsr: u32) -> Self {
        let mut next = if spsr & CPSR_TBIT != 0 {
            Self::thumb(target)
        } else {
            Self::arm(target)
        };
        next.changes_mode = true;
        next
    }

    fn exception(exception: Exception) -> Self {
        Self {
            pc: exception.vector(),
            thumb: false,
            exception: Some(exception),
            changes_mode: false,
        }
    }
}

/// The core state the simulator reads. PC is never read through this: the simulator knows the
/// address of the instruction it simulates.
pub(super) trait StepTarget {
    /// Read R0-R14 of the current mode.
    fn read_core_register(&mut self, register: u8) -> Result<u32, Arm7tdmiError>;
    /// Read the current mode's SPSR.
    fn read_current_spsr(&mut self) -> Result<u32, Arm7tdmiError>;
    /// Read a word-aligned 32-bit word from memory.
    fn read_memory_32(&mut self, address: u32) -> Result<u32, Arm7tdmiError>;
}

impl StepTarget for Arm7tdmiCommunicationInterface<'_> {
    fn read_core_register(&mut self, register: u8) -> Result<u32, Arm7tdmiError> {
        self.read_core_register(register)
    }

    fn read_current_spsr(&mut self) -> Result<u32, Arm7tdmiError> {
        self.read_spsr_preserving_r0()
    }

    fn read_memory_32(&mut self, address: u32) -> Result<u32, Arm7tdmiError> {
        self.read_memory_32(address)
    }
}

/// Evaluates an ARM/Thumb condition code (bits\[31:28\] of an ARM opcode, or the 4-bit condition
/// field of a Thumb conditional branch) against the N/Z/C/V flags in `cpsr`.
fn condition_passes(cond: u32, cpsr: u32) -> bool {
    let n = (cpsr >> 31) & 1 != 0;
    let z = (cpsr >> 30) & 1 != 0;
    let c = (cpsr >> 29) & 1 != 0;
    let v = (cpsr >> 28) & 1 != 0;
    match cond {
        0x0 => z,            // EQ
        0x1 => !z,           // NE
        0x2 => c,            // CS/HS
        0x3 => !c,           // CC/LO
        0x4 => n,            // MI
        0x5 => !n,           // PL
        0x6 => v,            // VS
        0x7 => !v,           // VC
        0x8 => c && !z,      // HI
        0x9 => !c || z,      // LS
        0xA => n == v,       // GE
        0xB => n != v,       // LT
        0xC => !z && n == v, // GT
        0xD => z || n != v,  // LE
        0xE => true,         // AL
        _ => false,          // NV: "never" on ARMv4
    }
}

/// Reads register `index` as an operand of the ARM instruction at `pc`. PC reads as the
/// instruction address + 8, or + 12 when the instruction shifts by a register (the register
/// read happens one cycle later).
fn arm_operand(
    target: &mut impl StepTarget,
    index: u32,
    pc: u32,
    shift_by_register: bool,
) -> Result<u32, Arm7tdmiError> {
    if index == 15 {
        Ok(pc.wrapping_add(if shift_by_register { 12 } else { 8 }))
    } else {
        target.read_core_register(index as u8)
    }
}

/// Applies an ARM barrel-shifter operation (`shift_type` 0-3 = LSL/LSR/ASR/ROR) to `value`.
///
/// `by_register` selects the register-specified form, where `amount` is the bottom byte of Rs
/// (an amount of 0 leaves the value unchanged, and amounts of 32 or more are meaningful).
/// Otherwise `amount` is the 5-bit immediate, where 0 encodes LSR #32/ASR #32/RRX.
fn barrel_shift(value: u32, shift_type: u32, amount: u32, by_register: bool, carry: bool) -> u32 {
    if by_register {
        match (shift_type, amount) {
            (_, 0) => value,
            (0, 1..=31) => value << amount,
            (0, _) => 0,
            (1, 1..=31) => value >> amount,
            (1, _) => 0,
            (2, 1..=31) => ((value as i32) >> amount) as u32,
            (2, _) => ((value as i32) >> 31) as u32,
            (_, amount) => value.rotate_right(amount % 32),
        }
    } else {
        match (shift_type, amount) {
            (0, amount) => value << amount,
            (1, 0) => 0,
            (1, amount) => value >> amount,
            (2, 0) => ((value as i32) >> 31) as u32,
            (2, amount) => ((value as i32) >> amount) as u32,
            (_, 0) => ((carry as u32) << 31) | (value >> 1), // RRX
            (_, amount) => value.rotate_right(amount),
        }
    }
}

/// Computes the register form of an ARM shifter operand (bits\[11:0\]: Rm, shifted by an
/// immediate or by Rs), as used by data-processing instructions and register-offset loads.
fn register_shifter_operand(
    target: &mut impl StepTarget,
    opcode: u32,
    pc: u32,
    cpsr: u32,
) -> Result<u32, Arm7tdmiError> {
    let by_register = (opcode >> 4) & 1 != 0;
    let rm_val = arm_operand(target, opcode & 0xF, pc, by_register)?;
    let shift_type = (opcode >> 5) & 0x3;
    let amount = if by_register {
        target.read_core_register(((opcode >> 8) & 0xF) as u8)? & 0xFF
    } else {
        (opcode >> 7) & 0x1F
    };
    let carry = (cpsr >> 29) & 1 != 0;
    Ok(barrel_shift(rm_val, shift_type, amount, by_register, carry))
}

pub(super) fn simulate_arm(
    target: &mut impl StepTarget,
    pc: u32,
    cpsr: u32,
) -> Result<NextInstruction, Arm7tdmiError> {
    let opcode = target.read_memory_32(pc)?;
    let sequential = NextInstruction::arm(pc.wrapping_add(4));

    if !condition_passes(opcode >> 28, cpsr) {
        return Ok(sequential);
    }

    match (opcode >> 25) & 0b111 {
        0b000 | 0b001 => {
            // BX: cond 0001 0010 1111 1111 1111 0001 Rm
            if (opcode & 0x0FFF_FFF0) == 0x012F_FF10 {
                let rm_val = arm_operand(target, opcode & 0xF, pc, false)?;
                return Ok(NextInstruction::interworking(rm_val));
            }
            // Multiplies, SWP and halfword/signed transfers (bits 7 and 4 set, register form):
            // their bits[15:12] aren't a data-processing Rd, and none of them may target PC.
            let immediate = (opcode >> 25) & 1 != 0;
            if !immediate && (opcode & 0x90) == 0x90 {
                return Ok(sequential);
            }
            // Data processing. TST/TEQ/CMP/CMN (opcodes 8-11) never write Rd. With S clear the
            // same space holds MRS/MSR (which don't write PC either; MSR may change the mode)
            // and, on later architectures, BLX Rm/CLZ/BKPT/QADD/SMLAxy. Decoded the way ARM7TDMI
            // silicon does it, including the SBZ/SBO bits the architecture leaves
            // unpredictable: bits[7:4] other than zero are undefined (BX is handled above);
            // otherwise MRS and MSR execute whatever their SBZ/SBO bits say (a register-form
            // MSR with bits[11:7] set even shifts its operand left by that amount).
            let dp_opcode = (opcode >> 21) & 0xF;
            let set_flags = (opcode >> 20) & 1 != 0;
            if (0x8..=0xB).contains(&dp_opcode) {
                if set_flags {
                    return Ok(sequential);
                }
                let is_msr = (opcode >> 21) & 1 != 0;
                let msr = NextInstruction {
                    changes_mode: true,
                    ..sequential
                };
                let undefined = NextInstruction::exception(Exception::Undefined);
                return Ok(match (immediate, is_msr) {
                    (true, true) => msr,
                    (true, false) => undefined,
                    (false, _) if (opcode >> 4) & 0xF != 0 => undefined,
                    (false, false) => sequential,
                    (false, true) => msr,
                });
            }
            if (opcode >> 12) & 0xF != 15 {
                return Ok(sequential);
            }

            let shift_by_register = !immediate && (opcode >> 4) & 1 != 0;
            let operand2 = if immediate {
                let rotate = ((opcode >> 8) & 0xF) * 2;
                (opcode & 0xFF).rotate_right(rotate)
            } else {
                register_shifter_operand(target, opcode, pc, cpsr)?
            };
            let rn_val = if dp_opcode == 0xD || dp_opcode == 0xF {
                0 // MOV/MVN don't use Rn
            } else {
                arm_operand(target, (opcode >> 16) & 0xF, pc, shift_by_register)?
            };

            let c = (cpsr >> 29) & 1;
            let result = match dp_opcode {
                0x0 => rn_val & operand2,                                 // AND
                0x1 => rn_val ^ operand2,                                 // EOR
                0x2 => rn_val.wrapping_sub(operand2),                     // SUB
                0x3 => operand2.wrapping_sub(rn_val),                     // RSB
                0x4 => rn_val.wrapping_add(operand2),                     // ADD
                0x5 => rn_val.wrapping_add(operand2).wrapping_add(c),     // ADC
                0x6 => rn_val.wrapping_sub(operand2).wrapping_sub(1 - c), // SBC
                0x7 => operand2.wrapping_sub(rn_val).wrapping_sub(1 - c), // RSC
                0xC => rn_val | operand2,                                 // ORR
                0xD => operand2,                                          // MOV
                0xE => rn_val & !operand2,                                // BIC
                0xF => !operand2,                                         // MVN
                _ => unreachable!(),
            };
            if set_flags {
                // `<op>S PC, ...` returns from an exception: CPSR is restored from SPSR.
                let spsr = target.read_current_spsr()?;
                Ok(NextInstruction::exception_return(result, spsr))
            } else {
                Ok(NextInstruction::arm(result))
            }
        }
        0b010 | 0b011 => {
            let register_offset = (opcode >> 25) & 1 != 0;
            if register_offset && (opcode >> 4) & 1 != 0 {
                // The undefined-instruction space of the ARM encoding.
                return Ok(NextInstruction::exception(Exception::Undefined));
            }
            // Single data transfer: cond 01 I P U B W L Rn Rd offset
            let load = (opcode >> 20) & 1 != 0;
            if !load || (opcode >> 12) & 0xF != 15 {
                return Ok(sequential);
            }
            let pre_index = (opcode >> 24) & 1 != 0;
            let add = (opcode >> 23) & 1 != 0;
            let rn_val = arm_operand(target, (opcode >> 16) & 0xF, pc, false)?;
            let offset = if register_offset {
                register_shifter_operand(target, opcode, pc, cpsr)?
            } else {
                opcode & 0xFFF
            };
            let address = if !pre_index {
                rn_val // post-indexed: load from the unmodified base
            } else if add {
                rn_val.wrapping_add(offset)
            } else {
                rn_val.wrapping_sub(offset)
            };
            // An unaligned LDR rotates the addressed word on ARMv4T. There is no interworking
            // on ARMv4T: the loaded value is always an ARM address.
            let value = target
                .read_memory_32(address & !0b11)?
                .rotate_right((address & 0b11) * 8);
            Ok(NextInstruction::arm(value))
        }
        0b100 => {
            // Block data transfer: cond 100 P U S W L Rn register_list
            let load = (opcode >> 20) & 1 != 0;
            let register_list = opcode & 0xFFFF;
            if !load || register_list & 0x8000 == 0 {
                return Ok(sequential);
            }
            let pre_index = (opcode >> 24) & 1 != 0;
            let add = (opcode >> 23) & 1 != 0;
            let rn_val = arm_operand(target, (opcode >> 16) & 0xF, pc, false)?;
            let count = register_list.count_ones();

            // The lowest address the transfer touches; registers are loaded in ascending
            // order from there, so PC (always the highest register) comes last.
            let lowest = match (pre_index, add) {
                (false, true) => rn_val,                                          // IA
                (true, true) => rn_val.wrapping_add(4),                           // IB
                (false, false) => rn_val.wrapping_sub(count * 4).wrapping_add(4), // DA
                (true, false) => rn_val.wrapping_sub(count * 4),                  // DB
            };
            let pc_address = lowest.wrapping_add((count - 1) * 4);
            let value = target.read_memory_32(pc_address & !0b11)?;
            if (opcode >> 22) & 1 != 0 {
                // `LDM {.., PC}^` also restores CPSR from SPSR.
                let spsr = target.read_current_spsr()?;
                Ok(NextInstruction::exception_return(value, spsr))
            } else {
                Ok(NextInstruction::arm(value))
            }
        }
        0b101 => {
            // B/BL: cond 101 L offset24
            let offset24 = opcode & 0x00FF_FFFF;
            let signed_offset = ((offset24 << 8) as i32) >> 6; // sign-extend, x4
            Ok(NextInstruction::arm(
                pc.wrapping_add(8).wrapping_add(signed_offset as u32),
            ))
        }
        0b110 => {
            // LDC/STC: ARM7TDMI has no coprocessors, so they are undefined.
            Ok(NextInstruction::exception(Exception::Undefined))
        }
        _ => {
            if (opcode >> 24) & 1 != 0 {
                Ok(NextInstruction::exception(Exception::SoftwareInterrupt))
            } else {
                // CDP/MCR/MRC: no coprocessors, undefined.
                Ok(NextInstruction::exception(Exception::Undefined))
            }
        }
    }
}

pub(super) fn simulate_thumb(
    target: &mut impl StepTarget,
    pc: u32,
    cpsr: u32,
) -> Result<NextInstruction, Arm7tdmiError> {
    let word = target.read_memory_32(pc & !0b11)?;
    let opcode = if pc & 0b10 == 0 {
        (word & 0xFFFF) as u16
    } else {
        (word >> 16) as u16
    };
    let sequential = NextInstruction::thumb(pc.wrapping_add(2));

    // Conditional branch: 1101 cccc offset8 (cond 1110 = undefined, 1111 = SWI).
    if (opcode & 0xF000) == 0xD000 {
        let cond = ((opcode >> 8) & 0xF) as u32;
        return Ok(match cond {
            0xE => NextInstruction::exception(Exception::Undefined),
            0xF => NextInstruction::exception(Exception::SoftwareInterrupt),
            _ if !condition_passes(cond, cpsr) => sequential,
            _ => {
                let offset8 = (opcode & 0xFF) as u32;
                let signed_offset = ((offset8 << 24) as i32) >> 23; // sign-extend 8-bit, x2
                NextInstruction::thumb(pc.wrapping_add(4).wrapping_add(signed_offset as u32))
            }
        });
    }

    match opcode & 0xF800 {
        // Unconditional branch: 11100 offset11
        0xE000 => {
            let offset11 = (opcode & 0x7FF) as u32;
            let signed_offset = ((offset11 << 21) as i32) >> 20; // sign-extend 11-bit, x2
            return Ok(NextInstruction::thumb(
                pc.wrapping_add(4).wrapping_add(signed_offset as u32),
            ));
        }
        // 11101: the BLX suffix on ARMv5, undefined on ARMv4T.
        0xE800 => return Ok(NextInstruction::exception(Exception::Undefined)),
        // BL prefix: only sets LR = PC + 4 + (offset << 12). The two halves of a BL are
        // separate instructions on ARMv4T, so a step stops at the suffix.
        0xF000 => return Ok(sequential),
        // BL suffix: branches to LR + (offset << 1) (LR as set by the prefix).
        0xF800 => {
            let lr = target.read_core_register(14)?;
            let offset_low = (opcode & 0x7FF) as u32;
            return Ok(NextInstruction::thumb(lr.wrapping_add(offset_low << 1)));
        }
        _ => {}
    }

    // BX: 010001 11 0 H2 Rm(3) 000
    if (opcode & 0xFF87) == 0x4700 {
        let rm = ((opcode >> 3) & 0xF) as u8;
        let rm_val = if rm == 15 {
            pc.wrapping_add(4)
        } else {
            target.read_core_register(rm)?
        };
        return Ok(NextInstruction::interworking(rm_val));
    }

    // POP {reglist, PC}: 1011 110 1 reglist(8). No interworking on ARMv4T: stays in Thumb.
    if (opcode & 0xFF00) == 0xBD00 {
        let reglist = (opcode & 0xFF) as u32;
        let sp = target.read_core_register(13)?;
        let value = target.read_memory_32(sp.wrapping_add(reglist.count_ones() * 4) & !0b11)?;
        return Ok(NextInstruction::thumb(value));
    }

    // Hi-register ADD/MOV with Rd == PC.
    if let Some(target_pc) =
        decode_hi_register_pc_target(opcode, pc, |rm| target.read_core_register(rm))?
    {
        return Ok(NextInstruction::thumb(target_pc));
    }

    Ok(sequential)
}

/// Decodes a Thumb hi-register operation (format 5: bits\[15:10\] = `0b010001`, Op in
/// bits\[9:8\]) and returns the branch target if it is an ADD or MOV with Rd = PC, or `None`
/// otherwise. BX (Op = 11) is handled separately by [`simulate_thumb`].
///
/// `resolve_rm` supplies Rm's value; it is only called when Rd = PC and Rm != PC.
///
/// `MOV PC, Rm` branches to Rm, `ADD PC, Rm` to PC + 4 + Rm. CMP never writes PC, even with
/// its Rn field set to PC.
fn decode_hi_register_pc_target(
    opcode: u16,
    pc: u32,
    resolve_rm: impl FnOnce(u8) -> Result<u32, Arm7tdmiError>,
) -> Result<Option<u32>, Arm7tdmiError> {
    if (opcode & 0xFC00) != 0x4400 {
        return Ok(None);
    }
    let op = (opcode >> 8) & 0b11;
    // CMP (op == 0b01) never writes Rd - not a branch, regardless of the encoded "Rd" field.
    if op != 0b00 && op != 0b10 {
        return Ok(None);
    }
    let is_add = op == 0b00;
    let h1 = (opcode >> 7) & 1;
    let rd_low = opcode & 0x7;
    let rd = (h1 << 3) | rd_low;
    if rd != 15 {
        return Ok(None);
    }
    let h2 = (opcode >> 6) & 1;
    let rm_low = (opcode >> 3) & 0x7;
    let rm = (h2 << 3) | rm_low;
    let rm_val = if rm == 15 {
        pc.wrapping_add(4)
    } else {
        resolve_rm(rm as u8)?
    };
    let result = if is_add {
        // Rd == PC, read mid-instruction: PC + 4 (pipeline), per Thumb convention.
        pc.wrapping_add(4).wrapping_add(rm_val)
    } else {
        rm_val
    };
    Ok(Some(result & !1))
}

/// Calculates where the next instruction executes by simulating the current one. Returns the
/// current PC, the current CPSR and the prediction.
pub(super) fn calculate_next_pc(
    interface: &mut Arm7tdmiCommunicationInterface,
) -> Result<(u32, u32, NextInstruction), Arm7tdmiError> {
    // Use the PC captured when the core halted: a fresh read would not return the halt-time
    // value (see `cached_halt_pc`), so the wrong instruction would be simulated.
    let pc = match interface.cached_halt_pc() {
        Some(pc) => pc,
        None => interface.read_core_register(15)?,
    };
    let cpsr = interface.read_core_register(16)?;

    let next = if cpsr & CPSR_TBIT != 0 {
        simulate_thumb(interface, pc, cpsr)?
    } else {
        simulate_arm(interface, pc, cpsr)?
    };
    Ok((pc, cpsr, next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A fake core: registers R0-R14, SPSR and a sparse word-addressed memory.
    #[derive(Default)]
    struct FakeTarget {
        regs: [u32; 15],
        spsr: u32,
        mem: HashMap<u32, u32>,
    }

    impl FakeTarget {
        fn with_code(address: u32, words: &[u32]) -> Self {
            let mut target = Self::default();
            for (i, &word) in words.iter().enumerate() {
                target.mem.insert(address + 4 * i as u32, word);
            }
            target
        }

        /// Place Thumb halfwords starting at `address` (which must be word aligned).
        fn with_thumb(address: u32, halfwords: &[u16]) -> Self {
            let mut target = Self::default();
            for (i, pair) in halfwords.chunks(2).enumerate() {
                let low = pair[0] as u32;
                let high = pair.get(1).copied().unwrap_or(0) as u32;
                target
                    .mem
                    .insert(address + 4 * i as u32, low | (high << 16));
            }
            target
        }
    }

    impl StepTarget for FakeTarget {
        fn read_core_register(&mut self, register: u8) -> Result<u32, Arm7tdmiError> {
            assert!(
                register < 15,
                "the simulator must not read PC as a register"
            );
            Ok(self.regs[register as usize])
        }

        fn read_current_spsr(&mut self) -> Result<u32, Arm7tdmiError> {
            Ok(self.spsr)
        }

        fn read_memory_32(&mut self, address: u32) -> Result<u32, Arm7tdmiError> {
            assert_eq!(address & 3, 0, "unaligned memory read {address:#x}");
            Ok(self.mem.get(&address).copied().unwrap_or(0))
        }
    }

    const PC: u32 = 0x40_0100;
    const Z: u32 = 1 << 30;
    const C: u32 = 1 << 29;
    const N: u32 = 1 << 31;
    const V: u32 = 1 << 28;

    fn arm(target: &mut FakeTarget, cpsr: u32) -> NextInstruction {
        simulate_arm(target, PC, cpsr).unwrap()
    }

    fn thumb(target: &mut FakeTarget, cpsr: u32) -> NextInstruction {
        simulate_thumb(target, PC, cpsr | CPSR_TBIT).unwrap()
    }

    #[test]
    fn condition_codes() {
        let cases = [
            (0x0, Z, true),
            (0x0, 0, false),
            (0x1, 0, true),
            (0x2, C, true),
            (0x3, C, false),
            (0x4, N, true),
            (0x5, N, false),
            (0x6, V, true),
            (0x7, V, false),
            (0x8, C, true),
            (0x8, C | Z, false),
            (0x9, Z, true),
            (0x9, C, false),
            (0xA, N | V, true),
            (0xA, N, false),
            (0xB, N, true),
            (0xB, N | V, false),
            (0xC, 0, true),
            (0xC, Z, false),
            (0xC, N, false),
            (0xD, Z, true),
            (0xD, V, true),
            (0xD, 0, false),
            (0xE, 0, true),
            (0xF, 0, false),
        ];
        for (cond, flags, expected) in cases {
            assert_eq!(condition_passes(cond, flags), expected, "cond {cond:#x}");
        }
    }

    #[test]
    fn arm_sequential_and_failed_condition() {
        // ADD R0, R0, #1
        assert_eq!(
            arm(&mut FakeTarget::with_code(PC, &[0xE280_0001]), 0),
            NextInstruction::arm(PC + 4)
        );
        // BEQ with Z clear falls through.
        assert_eq!(
            arm(&mut FakeTarget::with_code(PC, &[0x0A00_0010]), 0),
            NextInstruction::arm(PC + 4)
        );
    }

    #[test]
    fn arm_branches() {
        // B PC+8+0x40, and B . (offset -2 words)
        assert_eq!(
            arm(&mut FakeTarget::with_code(PC, &[0xEA00_0010]), 0).pc,
            PC + 8 + 0x40
        );
        assert_eq!(
            arm(&mut FakeTarget::with_code(PC, &[0xEAFF_FFFE]), 0).pc,
            PC
        );
        // BL backwards
        assert_eq!(
            arm(&mut FakeTarget::with_code(PC, &[0xEBFF_FFF0]), 0).pc,
            PC + 8 - 0x40
        );
    }

    #[test]
    fn arm_bx_selects_instruction_set() {
        let mut target = FakeTarget::with_code(PC, &[0xE12F_FF11]); // BX R1
        target.regs[1] = 0x40_0201;
        assert_eq!(arm(&mut target, 0), NextInstruction::thumb(0x40_0200));
        target.regs[1] = 0x40_0200;
        assert_eq!(arm(&mut target, 0), NextInstruction::arm(0x40_0200));
    }

    #[test]
    fn arm_data_processing_to_pc() {
        // MOV PC, LR
        let mut target = FakeTarget::with_code(PC, &[0xE1A0_F00E]);
        target.regs[14] = 0x40_1234;
        assert_eq!(arm(&mut target, 0), NextInstruction::arm(0x40_1234));

        // ADD PC, PC, R2, LSL #2 (jump table): PC reads as +8.
        let mut target = FakeTarget::with_code(PC, &[0xE08F_F102]);
        target.regs[2] = 3;
        assert_eq!(arm(&mut target, 0).pc, PC + 8 + 12);

        // ADD PC, PC, R2, LSL R3: with a register-specified shift PC reads as +12.
        let mut target = FakeTarget::with_code(PC, &[0xE08F_F312]);
        target.regs[2] = 1;
        target.regs[3] = 2;
        assert_eq!(arm(&mut target, 0).pc, PC + 12 + 4);

        // MOV PC, R2, LSR R3 with R3 = 0: a register shift by zero leaves the value unchanged.
        let mut target = FakeTarget::with_code(PC, &[0xE1A0_F332]);
        target.regs[2] = 0x40_0400;
        target.regs[3] = 0;
        assert_eq!(arm(&mut target, 0).pc, 0x40_0400);

        // MOV PC, R2, LSR R3 with R3 = 32: result 0.
        target.regs[3] = 32;
        assert_eq!(arm(&mut target, 0).pc, 0);

        // MOV PC, R2, RRX takes the carry flag into bit 31.
        let mut target = FakeTarget::with_code(PC, &[0xE1A0_F062]);
        target.regs[2] = 0x0080_0000;
        assert_eq!(arm(&mut target, C).pc, 0x8040_0000);
        assert_eq!(arm(&mut target, 0).pc, 0x0040_0000);

        // SUB PC, R1, #4 in ARM state clears the low two bits.
        let mut target = FakeTarget::with_code(PC, &[0xE241_F004]);
        target.regs[1] = 0x40_0106;
        assert_eq!(arm(&mut target, 0).pc, 0x40_0100);

        // CMP PC, #0 (Rd field = PC) and MSR must not be treated as PC writes.
        assert_eq!(
            arm(&mut FakeTarget::with_code(PC, &[0xE35F_0000]), 0).pc,
            PC + 4
        );
        let msr = arm(&mut FakeTarget::with_code(PC, &[0xE129_F000]), 0); // MSR CPSR_fc, R0
        assert_eq!(msr.pc, PC + 4);
        assert!(msr.changes_mode);
    }

    #[test]
    fn arm_exception_returns_take_state_from_spsr() {
        // SUBS PC, LR, #4 returning to Thumb code.
        let mut target = FakeTarget::with_code(PC, &[0xE25E_F004]);
        target.regs[14] = 0x40_0305;
        target.spsr = 0x3F; // SYS mode, T set
        let next = arm(&mut target, 0x12);
        assert_eq!(next.pc, 0x40_0300);
        assert!(next.thumb && next.changes_mode);

        // MOVS PC, LR returning to ARM code.
        let mut target = FakeTarget::with_code(PC, &[0xE1B0_F00E]);
        target.regs[14] = 0x40_0400;
        target.spsr = 0x1F;
        assert!(!arm(&mut target, 0x13).thumb);

        // LDMFD SP!, {R0, PC}^ to Thumb code.
        let mut target = FakeTarget::with_code(PC, &[0xE8FD_8001]);
        target.regs[13] = 0x40_2000;
        target.mem.insert(0x40_2004, 0x40_0501);
        target.spsr = 0x30;
        let next = arm(&mut target, 0x12);
        assert_eq!((next.pc, next.thumb), (0x40_0500, true));
    }

    #[test]
    fn arm_status_register_space() {
        let cpsr = 0x1F;
        for (opcode, expected) in [
            (0xE10F_0000u32, false), // MRS R0, CPSR
            (0xE14F_1000, false),    // MRS R1, SPSR
            (0xE10F_0101, false),    // MRS R0, CPSR, SBZ bits set: still MRS on silicon
            (0xE100_0000, false),    // MRS R0, CPSR, SBO bits clear: still MRS on silicon
            (0xE129_F000, true),     // MSR CPSR_fc, R0
            (0xE121_0000, true),     // MSR CPSR_c, R0, SBO bits clear: still MSR on silicon
            (0xE128_F100, true),     // MSR CPSR_f, R0 with SBZ bit 8 set: MSR (R0, LSL #2)
            (0xE321_F0D3, true),     // MSR CPSR_c, #0xD3
            (0xE112_0003, false),    // TST R2, R3 (S set)
        ] {
            let mut target = FakeTarget::with_code(PC, &[opcode]);
            let next = arm(&mut target, cpsr);
            assert_eq!(next.pc, PC + 4, "{opcode:#010x}");
            assert_eq!(next.exception, None, "{opcode:#010x}");
            assert_eq!(next.changes_mode, expected, "{opcode:#010x}");
        }
        // ARMv5+ encodings in the same space are undefined on ARMv4T silicon, and so is the
        // immediate form with S and bit 21 clear.
        for opcode in [
            0xE12F_FF33u32, // BLX R3
            0xE16F_1F11,    // CLZ R1, R1
            0xE120_0070,    // BKPT #0
            0xE102_0051,    // QADD R0, R1, R2
            0xE100_0081,    // SMLABB R0, R1, R0, R0
            0xE300_0000,    // immediate, opcode 8, S clear
            0xE121_F160,    // MSR CPSR_c, R0 with bits[7:4] set
        ] {
            let mut target = FakeTarget::with_code(PC, &[opcode]);
            assert_eq!(
                arm(&mut target, cpsr),
                NextInstruction::exception(Exception::Undefined),
                "{opcode:#010x}"
            );
        }
    }

    #[test]
    fn arm_multiply_is_not_data_processing() {
        // MLA R0, R1, R2, PC: bits[15:12] = 15 is the accumulate register, not a destination.
        assert_eq!(
            arm(&mut FakeTarget::with_code(PC, &[0xE020_F291]), 0).pc,
            PC + 4
        );
        // LDRH R0, [R1]: halfword transfer, not a data-processing instruction.
        assert_eq!(
            arm(&mut FakeTarget::with_code(PC, &[0xE1D1_00B0]), 0).pc,
            PC + 4
        );
    }

    #[test]
    fn arm_loads_to_pc() {
        // LDR PC, [PC, #0x18] (vector table idiom): loads from PC + 8 + 0x18.
        let mut target = FakeTarget::with_code(PC, &[0xE59F_F018]);
        target.mem.insert(PC + 8 + 0x18, 0x40_0800);
        assert_eq!(arm(&mut target, 0), NextInstruction::arm(0x40_0800));

        // LDR PC, [R1], #4 (post-indexed) loads from R1 itself; no interworking on ARMv4T.
        let mut target = FakeTarget::with_code(PC, &[0xE491_F004]);
        target.regs[1] = 0x40_2000;
        target.mem.insert(0x40_2000, 0x40_0901);
        assert_eq!(arm(&mut target, 0), NextInstruction::arm(0x40_0900));

        // LDR PC, [R1, -R2, LSL #2]
        let mut target = FakeTarget::with_code(PC, &[0xE711_F102]);
        target.regs[1] = 0x40_2010;
        target.regs[2] = 2;
        target.mem.insert(0x40_2008, 0x40_0A00);
        assert_eq!(arm(&mut target, 0).pc, 0x40_0A00);

        // LDMIA R0, {R1, R2, PC}, LDMIB, LDMDA, LDMDB
        for (opcode, pc_address) in [
            (0xE890_8006u32, 0x40_2008u32), // IA: base, base+4, base+8
            (0xE990_8006, 0x40_200C),       // IB
            (0xE810_8006, 0x40_2000),       // DA: base-8, base-4, base
            (0xE910_8006, 0x40_1FFC),       // DB: base-12, base-8, base-4
        ] {
            let mut target = FakeTarget::with_code(PC, &[opcode]);
            target.regs[0] = 0x40_2000;
            target.mem.insert(pc_address, 0x40_0B00);
            assert_eq!(arm(&mut target, 0).pc, 0x40_0B00, "opcode {opcode:#x}");
        }
    }

    #[test]
    fn arm_exceptions() {
        let swi = arm(&mut FakeTarget::with_code(PC, &[0xEF00_0000]), 0);
        assert_eq!(
            swi,
            NextInstruction::exception(Exception::SoftwareInterrupt)
        );
        assert_eq!(swi.pc, 0x08);
        // Architecturally undefined, coprocessor data operation, coprocessor load.
        for opcode in [0xE7F0_00F0u32, 0xEE00_0000, 0xED90_0000] {
            assert_eq!(
                arm(&mut FakeTarget::with_code(PC, &[opcode]), 0),
                NextInstruction::exception(Exception::Undefined),
                "opcode {opcode:#x}"
            );
        }
        // A SWI whose condition fails doesn't trap.
        assert_eq!(
            arm(&mut FakeTarget::with_code(PC, &[0x0F00_0000]), 0).pc,
            PC + 4
        );
    }

    #[test]
    fn thumb_branches() {
        // B . (0xE7FE), BNE forward with Z clear and set, B backwards.
        assert_eq!(thumb(&mut FakeTarget::with_thumb(PC, &[0xE7FE]), 0).pc, PC);
        assert_eq!(
            thumb(&mut FakeTarget::with_thumb(PC, &[0xD104]), 0).pc,
            PC + 4 + 8
        );
        assert_eq!(
            thumb(&mut FakeTarget::with_thumb(PC, &[0xD104]), Z).pc,
            PC + 2
        );
        assert_eq!(
            thumb(&mut FakeTarget::with_thumb(PC, &[0xE7F0]), 0).pc,
            PC + 4 - 0x20
        );
    }

    #[test]
    fn thumb_bl_halves_are_separate_instructions() {
        // BL +0x1000: prefix 0xF001, suffix 0xF800.
        let mut target = FakeTarget::with_thumb(PC, &[0xF001, 0xF800]);
        assert_eq!(thumb(&mut target, 0), NextInstruction::thumb(PC + 2));

        // At the suffix, the target is LR (set by the prefix) + offset.
        target.regs[14] = PC + 4 + 0x1000;
        let suffix = simulate_thumb(&mut target, PC + 2, CPSR_TBIT).unwrap();
        assert_eq!(suffix, NextInstruction::thumb(PC + 4 + 0x1000));

        // A suffix with a non-zero low offset, reached with an arbitrary LR.
        let mut target = FakeTarget::with_thumb(PC, &[0xF804]);
        target.regs[14] = 0x40_0801;
        assert_eq!(thumb(&mut target, 0).pc, 0x40_0808);
    }

    #[test]
    fn thumb_bx_selects_instruction_set() {
        // BX R1
        let mut target = FakeTarget::with_thumb(PC, &[0x4708]);
        target.regs[1] = 0x40_0200;
        assert_eq!(thumb(&mut target, 0), NextInstruction::arm(0x40_0200));
        target.regs[1] = 0x40_0301;
        assert_eq!(thumb(&mut target, 0), NextInstruction::thumb(0x40_0300));
        // BX PC (at a word-aligned address) switches to ARM at PC + 4.
        assert_eq!(
            thumb(&mut FakeTarget::with_thumb(PC, &[0x4778]), 0),
            NextInstruction::arm(PC + 4)
        );
        // BX LR
        let mut target = FakeTarget::with_thumb(PC, &[0x4770]);
        target.regs[14] = 0x40_0421;
        assert_eq!(thumb(&mut target, 0), NextInstruction::thumb(0x40_0420));
    }

    #[test]
    fn thumb_pop_pc_stays_in_thumb() {
        // POP {R4, PC}
        let mut target = FakeTarget::with_thumb(PC, &[0xBD10]);
        target.regs[13] = 0x40_2000;
        target.mem.insert(0x40_2004, 0x40_0551);
        assert_eq!(thumb(&mut target, 0), NextInstruction::thumb(0x40_0550));
    }

    #[test]
    fn thumb_second_halfword_is_decoded() {
        // Instruction in the upper half of a word: B . at PC + 2.
        let mut target = FakeTarget::with_thumb(PC, &[0x0000, 0xE7FE]);
        assert_eq!(
            simulate_thumb(&mut target, PC + 2, CPSR_TBIT).unwrap().pc,
            PC + 2
        );
    }

    #[test]
    fn thumb_exceptions() {
        assert_eq!(
            thumb(&mut FakeTarget::with_thumb(PC, &[0xDF12]), 0),
            NextInstruction::exception(Exception::SoftwareInterrupt)
        );
        for opcode in [0xDE00u16, 0xE800] {
            assert_eq!(
                thumb(&mut FakeTarget::with_thumb(PC, &[opcode]), 0),
                NextInstruction::exception(Exception::Undefined),
                "opcode {opcode:#x}"
            );
        }
    }

    #[test]
    fn exception_mode_mapping() {
        for exception in [
            Exception::Undefined,
            Exception::SoftwareInterrupt,
            Exception::DataAbort,
            Exception::Irq,
            Exception::Fiq,
        ] {
            assert_eq!(Exception::from_mode(exception.mode()), Some(exception));
        }
        assert_eq!(Exception::from_mode(0x1F), None);
        assert_eq!(Exception::from_mode(0x10), None);
    }

    /// `MOV PC, LR` (0x46F7) branches to LR.
    #[test]
    fn mov_pc_lr_targets_lr_value_exactly() {
        let target = decode_hi_register_pc_target(0x46F7, 0x400000, |rm| {
            assert_eq!(rm, 14, "should resolve LR (r14)");
            Ok(0x410010)
        })
        .unwrap();
        // Exactly LR, not `pc + 4 + lr` as for ADD.
        assert_eq!(target, Some(0x410010));
    }

    /// `ADD PC, R1` (0x448F) branches to PC + 4 + R1.
    #[test]
    fn add_pc_r1_uses_pc_plus_4_plus_rm() {
        let target = decode_hi_register_pc_target(0x448F, 0x400000, |rm| {
            assert_eq!(rm, 1);
            Ok(0x10)
        })
        .unwrap();
        assert_eq!(target, Some(0x400000 + 4 + 0x10));
    }

    /// `CMP PC, LR` (0x45F7) does not branch, although its Rn field is PC.
    #[test]
    fn cmp_opcode_never_redirects_control_flow() {
        let target = decode_hi_register_pc_target(0x45F7, 0x400000, |_| {
            panic!("must not read any register for a non-branching CMP")
        })
        .unwrap();
        assert_eq!(target, None);
    }

    /// `MOV R8, LR` (0x46F0) does not branch and reads no register.
    #[test]
    fn mov_to_non_pc_destination_is_not_a_branch() {
        let target = decode_hi_register_pc_target(0x46F0, 0x400000, |_| {
            panic!("must not read any register when Rd != PC")
        })
        .unwrap();
        assert_eq!(target, None);
    }

    /// `MOV PC, PC` (0x46FF) reads Rm as PC + 4 without a register read.
    #[test]
    fn mov_pc_pc_resolves_rm_as_pc_plus_4_without_a_register_read() {
        let target = decode_hi_register_pc_target(0x46FF, 0x400000, |_| {
            panic!("Rm == PC must use pc+4 directly, not a register read")
        })
        .unwrap();
        assert_eq!(target, Some(0x400000 + 4));
    }
}
