pub const fn rsr(opcode: u32, rs: u8, t: u8) -> u32 {
    opcode | (rs as u32) << 8 | (t as u32 & 0x0F) << 4
}
