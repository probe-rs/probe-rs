use crate::architecture::xtensa::arch::{CpuRegister, SpecialRegister};

pub mod format;

/// Loads a 32-bit word from the address in `src` into `DDR`
/// Note: this is an illegal instruction when the processor is not in On-Chip Debug Mode
pub const fn lddr32_p(src: CpuRegister) -> u32 {
    0x0070E0 | (src.address() as u32 & 0x0F) << 8
}

/// Reads special register `sr` into `t`
pub const fn rsr(sr: SpecialRegister, t: CpuRegister) -> u32 {
    format::rsr(0x030000, sr.address(), t.address())
}

/// Writes `t` into special register `sr`
pub const fn wsr(sr: SpecialRegister, t: CpuRegister) -> u32 {
    format::rsr(0x130000, sr.address(), t.address())
}

/// Returns the Core to the Running state
pub const fn rfdo(_i: u8) -> u32 {
    0xF1E000
}
