//! ARM Cortex-M specific constants

use std::{mem, ops::Range};

use gimli::LittleEndian;

pub const ADDRESS_SIZE: u8 = mem::size_of::<u32>() as u8;

/// According to Armv8-M Architecture Reference Manual, the most significant 8 bits are `0xFF` to
/// indicate `EXC_RETURN`, the rest is either reserved or contains data.
pub const EXC_RETURN_MARKER: u32 = 0xFF00_0000;

pub const EXC_RETURN_FTYPE_MASK: u32 = 1 << 4;

pub const ENDIANNESS: LittleEndian = LittleEndian;
pub type Endianness = LittleEndian;

const THUMB_BIT: u32 = 1;
// According to the ARM Cortex-M Reference Manual RAM memory must be located in this address range
// (vendors still place e.g. Core-Coupled RAM outside this address range)
pub const VALID_RAM_ADDRESS: Range<u32> = 0x2000_0000..0x4000_0000;

pub fn clear_thumb_bit(addr: u32) -> u32 {
    addr & !THUMB_BIT
}

/// Checks if PC is the HardFault handler
// XXX may want to relax this to cover the whole PC range of the `HardFault` handler
pub fn is_hard_fault(pc: u32, vector_table: &VectorTable) -> bool {
    subroutine_eq(pc, vector_table.hard_fault)
}

pub fn is_thumb_bit_set(addr: u32) -> bool {
    addr & THUMB_BIT == THUMB_BIT
}

pub fn set_thumb_bit(addr: u32) -> u32 {
    addr | THUMB_BIT
}

/// Checks if two subroutine addresses are equivalent by first clearing their `THUMB_BIT`
pub fn subroutine_eq(addr1: u32, addr2: u32) -> bool {
    addr1 & !THUMB_BIT == addr2 & !THUMB_BIT
}

/// The contents of the vector table
#[derive(Debug)]
pub struct VectorTable {
    // entry 0
    pub initial_stack_pointer: u32,
    // entry 3: HardFault handler
    pub hard_fault: u32,
}
