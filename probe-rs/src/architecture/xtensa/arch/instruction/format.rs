pub const fn rsr(opcode: u32, rs: u8, t: u8) -> u32 {
    opcode | (rs as u32) << 8 | (t as u32 & 0x0F) << 4
}

pub const fn rri8(opcode: u32, at: u8, _as: u8, off: u8) -> u32 {
    opcode | ((off as u32) << 16) | (_as as u32 & 0x0F) << 8 | (at as u32 & 0x0F) << 4
}
