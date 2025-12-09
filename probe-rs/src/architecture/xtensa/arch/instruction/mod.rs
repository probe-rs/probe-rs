use crate::architecture::xtensa::arch::{CpuRegister, SpecialRegister};

pub mod format;

#[derive(Clone, Copy, PartialEq, Debug)]
#[allow(dead_code)]
pub enum Instruction {
    /// Loads a 32-bit word from the address in `src` into `DDR`
    /// Note: this is an illegal instruction when the processor is not in On-Chip Debug Mode
    Lddr32P(CpuRegister),

    /// Stores a 32-bit word from `DDR` to the address in `src`
    /// Note: this is an illegal instruction when the processor is not in On-Chip Debug Mode
    Sddr32P(CpuRegister),

    /// Loads a 32-bit word from `s` to the address in `t`
    L32I(CpuRegister, CpuRegister, u8),

    /// Stores a 32-bit word from `s` to the address in `t`
    S32I(CpuRegister, CpuRegister, u8),

    /// Reads `SpecialRegister` into `CpuRegister`
    Rsr(SpecialRegister, CpuRegister),

    /// Writes `CpuRegister` into `SpecialRegister`
    Wsr(SpecialRegister, CpuRegister),

    /// Returns the Core to the Running state
    Rfdo(u8),

    /// Calls the function at the address in `CpuRegister` and instructs the next `entry` to rotate
    /// the register window by 8.
    CallX8(CpuRegister),

    /// Generates a debug exception
    Break(u8, u8),

    /// Rotate register window by n*4
    Rotw(u8),

    /// Execution synchronize
    Esync,
}

/// The architecture supports multi-word instructions. This enum represents the different encodings
// ... but we only support narrow ones for now
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum InstructionEncoding {
    /// Instruction encoding is narrow enough to fit into DIR0/DIR0EXEC
    Narrow(u32),
}

impl Instruction {
    const fn encode_bytes(self) -> (usize, u32) {
        let word = match self {
            Instruction::Lddr32P(src) => 0x0070E0 | ((src as u32 & 0x0F) << 8),
            Instruction::Sddr32P(src) => 0x0070F0 | ((src as u32 & 0x0F) << 8),
            Instruction::L32I(s, t, imm) => format::rri8(0x002002, s as u8, t as u8, imm),
            Instruction::S32I(s, t, imm) => format::rri8(0x006002, s as u8, t as u8, imm),
            Instruction::Rsr(sr, t) => format::rsr(0x030000, sr as u8, t as u8),
            Instruction::Wsr(sr, t) => format::rsr(0x130000, sr as u8, t as u8),
            Instruction::Break(s, t) => {
                // 0000 0000 0100 s t 0000
                format::rrr(0x000000, 4, s, t)
            }
            Instruction::CallX8(s) => format::callx(2, s as u8),
            Instruction::Rfdo(_) => 0xF1E000,
            Instruction::Rotw(count) => {
                // 0100 0000 1000 0000 t 0000
                format::rrr(0x400000, 8, 0, count)
            }
            Instruction::Esync => 0x002020,
        };

        (3, word)
    }

    pub const fn encode(self) -> InstructionEncoding {
        let narrow = self.encode_bytes().1;
        InstructionEncoding::Narrow(narrow)
    }
}
