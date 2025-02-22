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
        };

        (3, word)
    }

    /// Encodes the instruction into a Little Endian sequence of bytes and appends it to the given
    /// vector.
    pub fn encode_into_vec(self, vec: &mut Vec<u8>) {
        let (bytes, narrow) = self.encode_bytes();

        vec.extend_from_slice(&narrow.to_le_bytes()[..bytes]);
    }

    pub const fn encode(self) -> InstructionEncoding {
        let narrow = self.encode_bytes().1;
        InstructionEncoding::Narrow(narrow)
    }
}

pub(crate) fn into_binary(instructions: impl IntoIterator<Item = Instruction>) -> Vec<u8> {
    Program::instructions_into_bytes(instructions)
}

#[derive(Debug, Default)]
pub(crate) struct Program {
    bytes: Vec<u8>,
}

impl Program {
    // TODO:
    // - add origin address
    // - add ability to retrieve the current PC for jumps
    pub fn new() -> Self {
        Program { bytes: Vec::new() }
    }

    pub fn add_instruction(&mut self, instruction: Instruction) {
        instruction.encode_into_vec(&mut self.bytes);
    }

    pub fn add_instructions(&mut self, instructions: impl IntoIterator<Item = Instruction>) {
        for instruction in instructions {
            self.add_instruction(instruction);
        }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn instructions_into_bytes(instructions: impl IntoIterator<Item = Instruction>) -> Vec<u8> {
        let mut program = Program::new();

        program.add_instructions(instructions);

        program.into_bytes()
    }
}
