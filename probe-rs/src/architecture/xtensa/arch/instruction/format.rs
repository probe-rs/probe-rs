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

/// Implements the RRI8 instruction format.
pub const fn rri8(opcode: u32, at: u8, _as: u8, off: u8) -> u32 {
    opcode | ((off as u32) << 16) | (_as as u32 & 0x0F) << 8 | (at as u32 & 0x0F) << 4
}
