//! Instruction formats implemented as functions.
//!
//! Instruction formats are common bytecode formats used to simplify instruction implementation.
//! They are implemented with an opcode and a set of more-or-less standardised slots where
//! instructions may define their operands.
//!
//! For more information, see the Xtensa ISA documentation.

/// Implements the RSR instruction format.
pub const fn rsr(opcode: u32, rs: u8, t: u8) -> u32 {
    opcode | (rs as u32) << 8 | (t as u32 & 0x0F) << 4
}

/// Implements the RRR instruction format.
pub const fn rrr(opcode: u32, r: u8, s: u8, t: u8) -> u32 {
    (opcode & 0xFF000F)
        | ((r as u32 & 0x0F) << 12)
        | (s as u32 & 0x0F) << 8
        | (t as u32 & 0x0F) << 4
}

/// Implements the RRI8 instruction format.
pub const fn rri8(opcode: u32, s: u8, t: u8, imm8: u8) -> u32 {
    (opcode & 0x00FFFF) | (imm8 as u32) << 16 | ((s as u32 & 0x0F) << 8) | (t as u32 & 0x0F) << 4
}

/// Implements the CALLX instruction format.
pub const fn callx(window: u32, s: u8) -> u32 {
    0x0000C0 | ((window & 0x03) << 4) | (s as u32 & 0x0F) << 8
}
