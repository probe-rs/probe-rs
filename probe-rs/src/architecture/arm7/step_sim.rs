//! Minimal ARMv4T (ARM7TDMI) instruction simulator, used only to calculate the address of the
//! *next* instruction that will genuinely execute - so that [`super::Arm7tdmi::step`] can arm a
//! single, exact-match hardware breakpoint there instead of a wildcard "any address" watchpoint.
//!
//! The wildcard-watchpoint approach (matching every instruction fetch) was the original
//! implementation and looked reasonable on paper, but does not reliably catch exactly one
//! instruction on this silicon: repeated `step()` calls were observed advancing PC by a fixed,
//! large, constant amount (72 bytes) with LR/CPSR completely frozen - the same "RESTART executes
//! a bounded burst regardless of the real code" characteristic this whole architecture backend
//! has already had to work around once for the main free-run (`resume()`) path. The wildcard
//! watchpoint apparently never reliably wins the race against that bounded burst.
//!
//! OpenOCD's own ARM7/ARM9 `step` (`arm7_9_step` in `arm7_9_common.c`) does not rely on a
//! wildcard watchpoint either - it calls `arm_simulate_step` to decode/simulate the *current*
//! instruction and calculate the exact next PC (handling conditional execution, branches, and
//! any instruction that writes PC directly), then arms an exact-address breakpoint there before
//! letting the core run via the same bounded, single-shot "system speed access" primitive this
//! file already trusts elsewhere (`read_memory_32`/`write_memory_32`). This module is a
//! deliberately scoped port of that same technique for ARMv4T specifically (no Thumb-2, no VFP,
//! no BLX(immediate)/BLX(register) - ARMv4T doesn't have them) - it does not aim to simulate
//! every possible instruction's *effect*, only to answer "where does control flow go next",
//! matching OpenOCD's own "dry run" semantics: any instruction this module doesn't specifically
//! recognise is assumed not to redirect control flow, and PC simply advances by the instruction
//! size (this is also architecturally correct: MRS/MSR/CMP/TST/data-processing-without-Rd=PC/
//! plain LDR/STR-without-Rd=PC/STM/arithmetic-without-Rd=PC never redirect control flow on
//! their own).
//!
//! Known limitation, not handled: `MSR` changing CPSR's T (Thumb) bit directly (rather than via
//! `BX`/exception entry/exit) would change how the *next* instruction is decoded without moving
//! PC - vanishingly rare in practice (this backend's own code never does it), not implemented.

use super::communication_interface::{Arm7tdmiCommunicationInterface, Arm7tdmiError};

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
        _ => true,           // AL (0xE), and 0xF treated permissively (unpredictable pre-ARMv5)
    }
}

/// Computes an ARM "shifter operand" (the 12-bit `operand2` field of a data-processing or
/// single-data-transfer instruction), given the containing opcode, whether it's the
/// data-processing-style immediate-vs-register `I` bit convention (`is_dp_immediate_bit`) or the
/// inverted single-data-transfer convention, and a register-read callback.
fn shifter_operand(
    interface: &mut Arm7tdmiCommunicationInterface,
    opcode: u32,
    register_offset_form: bool,
) -> Result<u32, Arm7tdmiError> {
    if !register_offset_form {
        // Data-processing immediate: 8-bit immediate, rotated right by 2*rotate.
        let imm8 = opcode & 0xFF;
        let rotate = ((opcode >> 8) & 0xF) * 2;
        return Ok(imm8.rotate_right(rotate));
    }

    // Register form (used both by data-processing's non-immediate operand2, and by
    // single-data-transfer's register-offset form): Rm, optionally shifted.
    let rm_index = (opcode & 0xF) as u8;
    let mut rm_val = interface.read_core_register(rm_index)?;
    if rm_index == 15 {
        rm_val = rm_val.wrapping_add(8); // reading PC mid-instruction reads PC+8 (pipeline)
    }

    let shift_type = (opcode >> 5) & 0x3;
    let shift_amount = if (opcode >> 4) & 1 == 0 {
        (opcode >> 7) & 0x1F
    } else {
        // Shift amount taken from a register's bottom byte - rare in practice, but handled.
        let rs_index = ((opcode >> 8) & 0xF) as u8;
        interface.read_core_register(rs_index)? & 0xFF
    };

    Ok(match shift_type {
        0 => rm_val.wrapping_shl(shift_amount), // LSL
        1 => {
            if shift_amount == 0 {
                0
            } else {
                rm_val.wrapping_shr(shift_amount)
            }
        } // LSR
        2 => {
            let amount = if shift_amount == 0 { 31 } else { shift_amount };
            ((rm_val as i32).wrapping_shr(amount)) as u32
        } // ASR
        3 => {
            if shift_amount == 0 {
                rm_val >> 1 // RRX not modelled precisely (would need carry-in) - close enough
            } else {
                rm_val.rotate_right(shift_amount)
            }
        } // ROR
        _ => unreachable!(),
    })
}

fn simulate_arm(
    interface: &mut Arm7tdmiCommunicationInterface,
    pc: u32,
    cpsr: u32,
) -> Result<u32, Arm7tdmiError> {
    let opcode = interface.read_memory_32(pc)?;
    let cond = opcode >> 28;

    if !condition_passes(cond, cpsr) {
        return Ok(pc.wrapping_add(4));
    }

    // BX (register branch/exchange): cond 0001 0010 1111 1111 1111 0001 Rm
    if (opcode & 0x0FFF_FFF0) == 0x012F_FF10 {
        let rm_index = (opcode & 0xF) as u8;
        let mut target = interface.read_core_register(rm_index)?;
        if rm_index == 15 {
            target = pc.wrapping_add(8);
        }
        return Ok(target & !1);
    }

    // Branch/Branch-with-link: cond 101 L offset24
    if (opcode >> 25) & 0x7 == 0b101 {
        let offset24 = opcode & 0x00FF_FFFF;
        let signed_offset = ((offset24 << 8) as i32) >> 6; // sign-extend 24-bit word offset, x4
        return Ok(pc.wrapping_add(8).wrapping_add(signed_offset as u32));
    }

    // Data-processing: cond 00 I opcode S Rn Rd operand2
    if (opcode >> 26) & 0x3 == 0b00 {
        let rd = (opcode >> 12) & 0xF;
        if rd == 15 {
            let dp_opcode = (opcode >> 21) & 0xF;
            // Exclude TST/TEQ/CMP/CMN (1000-1011) - these never write Rd, and this bit
            // pattern with Rd field = 1111 is also how MRS/MSR are encoded (their "Rd"-shaped
            // field isn't a real destination register there either).
            if !(0x8..=0xB).contains(&dp_opcode) {
                let is_immediate = (opcode >> 25) & 1 != 0;
                let operand2 = shifter_operand(interface, opcode, !is_immediate)?;

                let rn = (opcode >> 16) & 0xF;
                let rn_val = if dp_opcode == 0xD || dp_opcode == 0xF {
                    0 // MOV/MVN don't use Rn at all
                } else if rn == 15 {
                    pc.wrapping_add(8)
                } else {
                    interface.read_core_register(rn as u8)?
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
                return Ok(result & !1);
            }
        }
        return Ok(pc.wrapping_add(4));
    }

    // Single data transfer (LDR): cond 01 I P U B W L Rn Rd offset
    if (opcode >> 26) & 0x3 == 0b01 {
        let load = (opcode >> 20) & 1 != 0;
        let rd = (opcode >> 12) & 0xF;
        if load && rd == 15 {
            let register_offset = (opcode >> 25) & 1 != 0; // inverted vs. data-processing's I
            let pre_index = (opcode >> 24) & 1 != 0;
            let add = (opcode >> 23) & 1 != 0;
            let rn = (opcode >> 16) & 0xF;
            let rn_val = if rn == 15 {
                pc.wrapping_add(8)
            } else {
                interface.read_core_register(rn as u8)?
            };

            let offset = if register_offset {
                shifter_operand(interface, opcode, true)?
            } else {
                opcode & 0xFFF
            };

            let load_address = if pre_index {
                if add {
                    rn_val.wrapping_add(offset)
                } else {
                    rn_val.wrapping_sub(offset)
                }
            } else {
                rn_val // post-indexed: load from the unmodified base
            };

            let load_value = interface.read_memory_32(load_address & !0b11)?;
            return Ok(load_value & !1);
        }
        return Ok(pc.wrapping_add(4));
    }

    // Block data transfer (LDM/STM): cond 100 P U S W L Rn register_list
    if (opcode >> 25) & 0x7 == 0b100 {
        let load = (opcode >> 20) & 1 != 0;
        let register_list = opcode & 0xFFFF;
        if load && (register_list & 0x8000) != 0 {
            let pre_index = (opcode >> 24) & 1 != 0;
            let add = (opcode >> 23) & 1 != 0;
            let rn = (opcode >> 16) & 0xF;
            let rn_val = interface.read_core_register(rn as u8)?;
            let bits_set = register_list.count_ones();

            let base = match (pre_index, add) {
                (false, true) => rn_val,                                             // IA
                (true, true) => rn_val.wrapping_add(4),                              // IB
                (false, false) => rn_val.wrapping_sub(bits_set * 4).wrapping_add(4), // DA
                (true, false) => rn_val.wrapping_sub(bits_set * 4),                  // DB
            };

            // PC (bit 15) is always the highest, last-loaded register in the list.
            let pc_slot_index = (register_list & 0x7FFF).count_ones();
            let pc_load_address = base.wrapping_add(pc_slot_index * 4);
            let load_value = interface.read_memory_32(pc_load_address & !0b11)?;
            return Ok(load_value & !1);
        }
        return Ok(pc.wrapping_add(4));
    }

    // Anything else (MRS/MSR/CMP/TST/TEQ/CMN/STR/STM/SWI/coprocessor/...) never redirects
    // control flow on its own.
    Ok(pc.wrapping_add(4))
}

fn simulate_thumb(
    interface: &mut Arm7tdmiCommunicationInterface,
    pc: u32,
    cpsr: u32,
) -> Result<u32, Arm7tdmiError> {
    let word = interface.read_memory_32(pc & !0b11)?;
    let opcode = if pc & 0b10 == 0 {
        (word & 0xFFFF) as u16
    } else {
        (word >> 16) as u16
    };

    // Conditional branch: 1101 cccc offset8 (cond 1110 = undefined, 1111 = SWI - not a branch)
    if (opcode & 0xF000) == 0xD000 {
        let cond = ((opcode >> 8) & 0xF) as u32;
        if cond < 0xE {
            if !condition_passes(cond, cpsr) {
                return Ok(pc.wrapping_add(2));
            }
            let offset8 = (opcode & 0xFF) as u32;
            let signed_offset = ((offset8 << 24) as i32) >> 23; // sign-extend 8-bit, x2
            return Ok(pc.wrapping_add(4).wrapping_add(signed_offset as u32));
        }
    }

    // Unconditional branch: 11100 offset11
    if (opcode & 0xF800) == 0xE000 {
        let offset11 = (opcode & 0x7FF) as u32;
        let signed_offset = ((offset11 << 21) as i32) >> 20; // sign-extend 11-bit, x2
        return Ok(pc.wrapping_add(4).wrapping_add(signed_offset as u32));
    }

    // BL/BLX prefix+suffix (2-halfword): 11110 offset_high(11), then 11111 offset_low(11).
    // ARMv4T only has BL (suffix 11111), not BLX (suffix 11101, ARMv5+).
    if (opcode & 0xF800) == 0xF000 {
        let next_word_addr = pc.wrapping_add(2);
        let next_full = interface.read_memory_32(next_word_addr & !0b11)?;
        let opcode2 = if next_word_addr & 0b10 == 0 {
            (next_full & 0xFFFF) as u16
        } else {
            (next_full >> 16) as u16
        };
        if (opcode2 & 0xF800) == 0xF800 {
            let offset_high = (opcode & 0x7FF) as u32;
            let offset_low = (opcode2 & 0x7FF) as u32;
            let signed_high = ((offset_high << 21) as i32) >> 9; // sign-extend 11-bit, <<12
            let target = pc
                .wrapping_add(4)
                .wrapping_add(signed_high as u32)
                .wrapping_add(offset_low << 1);
            return Ok(target & !1);
        }
        // Not a real BL pair (or a BLX we don't support on ARMv4T) - fall through as plain.
    }

    // BX/BLX (register): 01000111 L Rm(4) 000
    if (opcode & 0xFF87) == 0x4700 {
        let rm_index = ((opcode >> 3) & 0xF) as u8;
        let mut target = interface.read_core_register(rm_index)?;
        if rm_index == 15 {
            target = pc.wrapping_add(4);
        }
        return Ok(target & !1);
    }

    // POP {reglist, PC}: 1011 110 1 reglist(8)
    if (opcode & 0xFF00) == 0xBD00 {
        let reglist = (opcode & 0xFF) as u32;
        let sp = interface.read_core_register(13)?;
        let pc_load_address = sp.wrapping_add(reglist.count_ones() * 4);
        let load_value = interface.read_memory_32(pc_load_address & !0b11)?;
        return Ok(load_value & !1);
    }

    // Hi-register data-processing (format 5) with Rd == PC - see `decode_hi_register_pc_target`'s
    // own doc comment for the encoding and the 2026-09-10 bug this replaced.
    if let Some(target) =
        decode_hi_register_pc_target(opcode, pc, |rm| interface.read_core_register(rm))?
    {
        return Ok(target);
    }

    // Everything else advances normally.
    Ok(pc.wrapping_add(2))
}

/// Decodes a Thumb hi-register data-processing instruction (format 5: fixed bits[15:10] =
/// `0b010001`, mask `0xFC00`, value `0x4400`, with a 2-bit Op field at bits[9:8] distinguishing
/// ADD(00)/CMP(01)/MOV(10) - BX(11) is handled by a separate, earlier check in
/// [`simulate_thumb`], so it never reaches here) whose destination is PC (R15), returning the
/// address execution will redirect to - or `None` if `opcode` isn't this shape, or its
/// destination isn't PC (in which case, per this module's own doc comment, it doesn't redirect
/// control flow at all and the caller should just advance PC normally).
///
/// `resolve_rm` supplies Rm's value (only ever invoked when the decode actually needs it, i.e.
/// Rd == PC and Rm != PC itself) - a real caller reads the live register; this indirection keeps
/// the decode logic itself pure and directly unit-testable with a literal value, no hardware
/// needed (see this module's `tests` below).
///
/// BUG FOUND (2026-09-10), via `cargo clippy`'s `bad_bit_mask` deny-lint: the previous form of
/// this check (`(opcode & 0xFC00) == 0x4400 || (opcode & 0xFC00) == 0x4600`) could never take its
/// second branch at all - `0xFC00` doesn't cover bit 9, the low bit of the Op field, so a masked
/// opcode can never equal `0x4600` (which has that bit set) - clippy correctly flagged this arm
/// as dead code. Worse than just dead code: because the *first* branch's mask also doesn't cover
/// the Op field, it silently matched ADD, CMP, *and* MOV alike (any Op value), and the resulting
/// `is_add` flag then re-evaluated that same always-true condition - so every format-5
/// instruction with Rd/H1 == PC that reached here (an actual `MOV PC, Rm` return idiom included,
/// e.g. `MOV PC, LR`) was computed as if it were `ADD Rd(=PC), Rm` (`pc+4+rm_val`), not the real
/// `MOV` (`rm_val` alone) - a wrong predicted next-PC for a genuinely common ARM idiom. `CMP`
/// reaching here (Rd is really an "Rn" compare operand and never redirects control flow, as this
/// whole module's own doc comment already specifies) was wrongly treated as a branch too. Fixed
/// by extracting Op explicitly and only taking the two operations that can write PC.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `MOV PC, LR` = 0x46F7 - the exact real-world idiom the 2026-09-10 bug mispredicted.
    #[test]
    fn mov_pc_lr_targets_lr_value_exactly() {
        let target = decode_hi_register_pc_target(0x46F7, 0x400000, |rm| {
            assert_eq!(rm, 14, "should resolve LR (r14)");
            Ok(0x410010)
        })
        .unwrap();
        // Must be exactly the LR value - NOT `pc + 4 + lr` (the pre-fix, ADD-shaped bug).
        assert_eq!(target, Some(0x410010));
    }

    /// `ADD PC, R1` (Rd=PC so H1=1, Rm=R1 so H2=0) = 0x448F - the one case that really should
    /// use the `pc + 4 + rm` formula.
    #[test]
    fn add_pc_r1_uses_pc_plus_4_plus_rm() {
        let target = decode_hi_register_pc_target(0x448F, 0x400000, |rm| {
            assert_eq!(rm, 1);
            Ok(0x10)
        })
        .unwrap();
        assert_eq!(target, Some(0x400000 + 4 + 0x10));
    }

    /// `CMP PC, LR` (Op=01, "Rd" field = PC, Rm = LR) = 0x45F7 - deliberately chosen with the
    /// "Rd" field equal to PC too, so this doesn't just avoid the ADD/MOV branch by having a
    /// non-PC destination - it proves the CMP-never-writes-PC guard fires *before* any
    /// destination check would otherwise have mattered.
    #[test]
    fn cmp_opcode_never_redirects_control_flow() {
        let target = decode_hi_register_pc_target(0x45F7, 0x400000, |_| {
            panic!("must not read any register for a non-branching CMP")
        })
        .unwrap();
        assert_eq!(target, None);
    }

    /// `MOV R8, LR` (Rd=R8, not PC) - a real, extremely common hi-register move that must not be
    /// treated as a branch, and must not need any register read at all (mirrors the "only read
    /// Rm when Rd == PC" optimization the real caller relies on).
    #[test]
    fn mov_to_non_pc_destination_is_not_a_branch() {
        let target = decode_hi_register_pc_target(0x46F0, 0x400000, |_| {
            panic!("must not read any register when Rd != PC")
        })
        .unwrap();
        assert_eq!(target, None);
    }

    /// `MOV PC, PC` (Rm == PC too) - Rm must resolve to `pc + 4` (the live architectural PC read
    /// convention), not trigger a register read at all.
    #[test]
    fn mov_pc_pc_resolves_rm_as_pc_plus_4_without_a_register_read() {
        let target = decode_hi_register_pc_target(0x46FF, 0x400000, |_| {
            panic!("Rm == PC must use pc+4 directly, not a register read")
        })
        .unwrap();
        assert_eq!(target, Some(0x400000 + 4));
    }
}

/// Calculates the address of the next instruction that will genuinely execute, by decoding and
/// simulating the *current* one - see the module doc comment for why this replaces a wildcard
/// watchpoint for single-stepping on this core.
pub(super) fn calculate_next_pc(
    interface: &mut Arm7tdmiCommunicationInterface,
) -> Result<(u32, u32), Arm7tdmiError> {
    let pc = interface.read_core_register(15)?;
    let cpsr = interface.read_core_register(16)?;
    let thumb = (cpsr >> 5) & 1 != 0;

    let next_pc = if thumb {
        simulate_thumb(interface, pc, cpsr)?
    } else {
        simulate_arm(interface, pc, cpsr)?
    };
    Ok((pc, next_pc))
}
