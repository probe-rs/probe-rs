/// RISCV breakpoint instruction
pub const EBREAK: u32 = 0b000000000001_00000_000_00000_1110011;

/// Assemble a `lw` instruction.
pub const fn lw(offset: u32, base: u32, width: u32, destination: u32) -> u32 {
    let opcode = 0b000_0011;

    offset << 20 | base << 15 | width << 12 | destination << 7 | opcode
}

/// Assemble a `sw` instruction.
pub const fn sw(offset: u32, base: u32, width: u32, source: u32) -> u32 {
    let opcode = 0b010_0011;

    let offset_lower = offset & 0b11111;
    let offset_upper = offset >> 5;

    offset_upper << 25 | source << 20 | base << 15 | width << 12 | offset_lower << 7 | opcode
}

/// Assemble a `addi` instruction.
pub const fn addi(source: u32, destination: u32, immediate: u32) -> u32 {
    let opcode = 0b001_0011;
    let function = 0b000;

    immediate << 20 | source << 15 | function << 12 | destination << 7 | opcode
}
