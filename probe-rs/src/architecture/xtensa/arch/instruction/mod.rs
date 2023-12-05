use crate::architecture::xtensa::arch::{CpuRegister, SpecialRegister};

pub mod format;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Instruction {
    /// Loads a 32-bit word from the address in `src` into `DDR`
    /// Note: this is an illegal instruction when the processor is not in On-Chip Debug Mode
    Lddr32P(CpuRegister),

    /// Stores a 32-bit word from `DDR` to the address in `src`
    /// Note: this is an illegal instruction when the processor is not in On-Chip Debug Mode
    Sddr32P(CpuRegister),

    /// Stores 8 bits from `at` to the address in `as` offset by a constant.
    ///
    /// This instruction can not access InstrRAM.
    S8i(CpuRegister, CpuRegister, u8),

    /// Reads `SpecialRegister` into `CpuRegister`
    Rsr(SpecialRegister, CpuRegister),

    /// Writes `CpuRegister` into `SpecialRegister`
    Wsr(SpecialRegister, CpuRegister),

    /// Invalidates the I-Cache at the address in `CpuRegister` + offset.
    ///
    /// The offset will be divided by 4 and has a maximum value of 1020.
    Ihi(CpuRegister, u32),

    /// Writes back and Invalidates the D-Cache at the address in `CpuRegister` + offset.
    ///
    /// The offset will be divided by 4 and has a maximum value of 1020.
    Dhwbi(CpuRegister, u32),

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
            Instruction::Sddr32P(src) => 0x0070F0 | (src.address() as u32 & 0x0F) << 8,
            Instruction::Rsr(sr, t) => format::rsr(0x030000, sr.address(), t.address()),
            Instruction::Wsr(sr, t) => format::rsr(0x130000, sr.address(), t.address()),
            Instruction::S8i(at, as_, offset) => {
                format::rri8(0x004002, at.address(), as_.address(), offset)
            }
            Instruction::Ihi(src, offset) => {
                format::rri8(0x0070E2, 0, src.address(), (offset / 4) as u8)
            }
            Instruction::Dhwbi(src, offset) => {
                format::rri8(0x007052, 0, src.address(), (offset / 4) as u8)
            }
            Instruction::Rfdo(_) => 0xF1E000,
        };

        InstructionEncoding::Narrow(narrow)
    }
}
