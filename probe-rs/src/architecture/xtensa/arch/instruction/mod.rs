use crate::architecture::xtensa::arch::{CpuRegister, SpecialRegister};

pub mod format;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Instruction {
    /// Loads a 32-bit word from the address in `src` into `DDR`
    /// Note: this is an illegal instruction when the processor is not in On-Chip Debug Mode
    Lddr32P(CpuRegister),

    /// Reads special register `sr` into `t`
    Rsr(SpecialRegister, CpuRegister),

    /// Writes `t` into special register `sr`
    Wsr(SpecialRegister, CpuRegister),

    /// Returns the Core to the Running state
    Rfdo(u8),
}

/// The architecture supports multi-word instructions. This enum represents the different encodings
// ... but we only support narrow ones for now
pub enum InstructionEncoding {
    /// Instruction encoding is narrow enough to fit into DIR0/DIR0EXEC
    Narrow(u32),
}

impl Instruction {
    pub fn encode(self) -> InstructionEncoding {
        let narrow = match self {
            Instruction::Lddr32P(src) => 0x0070E0 | (src.address() as u32 & 0x0F) << 8,
            Instruction::Rsr(sr, t) => format::rsr(0x030000, sr.address(), t.address()),
            Instruction::Wsr(sr, t) => format::rsr(0x130000, sr.address(), t.address()),
            Instruction::Rfdo(_) => 0xF1E000,
        };

        InstructionEncoding::Narrow(narrow)
    }
}
