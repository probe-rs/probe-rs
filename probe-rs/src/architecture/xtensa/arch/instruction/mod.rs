pub mod format;

/// Loads a 32-bit word from the address in `src` into `DDR`
/// Note: this is an illegal instruction when the processor is not in On-Chip Debug Mode
pub const fn lddr32_p(src: u8) -> u32 {
    0x0070E0 | (src as u32 & 0x0F) << 8
}

/// Reads special register `sr` into `t`
pub const fn rsr(sr: u8, t: u8) -> u32 {
    format::rsr(0x030000, sr, t)
}

/// Writes `t` into special register `sr`
pub const fn wsr(sr: u8, t: u8) -> u32 {
    format::rsr(0x130000, sr, t)
}

/// Returns the Core to the Running state
pub const fn rfdo(_i: u8) -> u32 {
    0xF1E000
}
