#![allow(clippy::unusual_byte_groupings)]

/// RISCV breakpoint instruction
pub const EBREAK: u32 = 0b000000000001_00000_000_00000_1110011;

/// Assemble a `lw` instruction.
pub fn lw(offset: u16, base: u8, width: u8, destination: u8) -> u32 {
    let opcode = 0b000_0011;

    i_type_instruction(opcode, base, width, destination, offset)
}

/// Assemble a `sw` instruction.
pub const fn sw(offset: u32, base: u32, width: u32, source: u32) -> u32 {
    let opcode = 0b010_0011;

    let offset_lower = offset & 0b11111;
    let offset_upper = offset >> 5;

    offset_upper << 25 | source << 20 | base << 15 | width << 12 | offset_lower << 7 | opcode
}

/// Assemble a `addi` instruction.
pub fn addi(source: u8, destination: u8, immediate: u16) -> u32 {
    let opcode = 0b001_0011;
    let function = 0b000;

    i_type_instruction(opcode, source, function, destination, immediate)
}

// We need to perform the csrr instruction, which reads a CSR.
// This is a pseudo instruction, which actually is encoded as a
// csrrs instruction, with the rs1 register being x0,
// so no bits are changed in the CSR, but the CSR is read into rd, i.e. s0.
pub fn csrr(rd: u8, csr: u16) -> u32 {
    csrrs(rd, 0, csr)
}

/// Assemble a `csrrs` instruction
pub fn csrrs(rd: u8, rs1: u8, csr: u16) -> u32 {
    let opcode = 0b1110011;
    let funct3 = 0b010;
    i_type_instruction(opcode, rs1, funct3, rd, csr)
}

// We need to perform the csrw instruction, which writes a CSR.
// This is a pseudo instruction, which actually is encoded as a
// csrrw instruction, with the destination register being x0,
// so the read is ignored.
pub fn csrw(csr: u16, rs: u8) -> u32 {
    csrrw(0, rs, csr)
}

pub fn csrrw(rd: u8, rs1: u8, csr: u16) -> u32 {
    let opcode = 0b1110011;
    let funct3 = 0b001;

    i_type_instruction(opcode, rs1, funct3, rd, csr)
}

/// Assemble an I-type instruction, as specified in the RISCV ISA
///
/// This function panics if any of the values would have to be truncated.
fn i_type_instruction(opcode: u8, rs1: u8, funct3: u8, rd: u8, imm: u16) -> u32 {
    assert!(opcode <= 0x7f); // [06:00]
    assert!(rd <= 0x1f); // [11:07]
    assert!(funct3 <= 0x7); // [14:12]
    assert!(rs1 <= 0x1f); // [19:15]
    assert!(imm <= 0xfff); // [31:20]

    (imm as u32) << 20
        | (rs1 as u32) << 15
        | (funct3 as u32) << 12
        | (rd as u32) << 7
        | opcode as u32
}

#[cfg(test)]
mod test {
    use super::{csrr, csrw, lw, sw};

    #[test]
    fn assemble_csrr() {
        // Assembly output of assembly 'csrr  s0, mie'
        //
        // mie address: 0x304
        // s0 index:    8
        let expected = 0x30402473;

        let assembled = csrr(8, 0x304);

        assert_eq!(assembled, expected);
    }

    #[test]
    fn assemble_csrw() {
        // Assembly output of assembly 'csrw  mstatus, s1'
        //
        // mstatus address: 0x300
        // s9 index:    9
        let expected = 0x30049073;

        let assembled = csrw(0x300, 9);

        assert_eq!(assembled, expected);
    }

    #[test]
    fn assemble_sw() {
        // Assembly output of assembly 'sw      x1, 4(x2)'
        //
        let expected = 0x00112223;

        let assembled = sw(4, 2, 2, 1);

        assert_eq!(assembled, expected);
    }

    #[test]
    fn assemble_lw() {
        // Assembly output of assembly 'lw      x3, 8(x4)'
        //
        let expected = 0x00822183;

        let assembled = lw(8, 4, 2, 3);

        assert_eq!(assembled, expected);
    }
}
