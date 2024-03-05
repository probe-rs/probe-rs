use super::{super::ColumnType, instruction::Instruction};
use std::{num::NonZeroU64, ops::RangeInclusive};

/// The concept of an instruction block is based on
/// [Rust's MIR basic block definition](https://rustc-dev-guide.rust-lang.org/appendix/background.html#cfg)
/// The concept is also a close match for how the DAP specification defines the a `statement`
/// [SteppingGranularity](https://microsoft.github.io/debug-adapter-protocol/specification#Types_SteppingGranularity)
/// In the context of the `probe-rs` debugger, an instruction block is a contiguous series of instructions
/// which belong to a single [`Sequence`].
/// The key difference between instructions in a block, and those in a [`gimli::LineSequence`], is that we can rely
/// on the 'next' instruction in the block to be the 'next' instruction the processor will execute (barring any interrupts).
/// ### Implementation discussion:
/// Indentifying the boundaries of each [`Block`] is the key to identifying valid halt locations, and is the primary
/// purpose of the [`Block`] struct. Current versions of Rust (up to rustc 1.76.0) does not populate the
/// `DW_LNS_basic_block` attribute of the line program rows in the DWARF debug information. The implication of this is that
/// we need to infer the boundaries of each block withing the sequence of instructions, from other blocks, as well as
/// from the prologue and epilogue markers. The approach taken is as follows:
/// - The first block is the prologue block, and is identified by the `DW_LNS_set_prologue_end` attribute.
/// - If the sequence starting address is a non-inlined function, then if the DWARF `DW_AT_subprogram` attribute
///   for the function uses:
///   - `DW_AT_ranges`, we use those ranges as initial block boundaries. These ranges only covers
///      parts of the sequence, and we start by creating a block for each covered range, and blocks
///      for the remaining covered ranges.
/// - To facilitate 'stepping', we also need to identify how blocks transition from one to the next,
///   and unlike inside a sequence, these are typicall not sequential addresses.
///   These addresses may be unknown (`None`), in which case our ability step through
///   the sequence may be limited.
#[derive(Default)]
pub(crate) struct Block {
    pub(crate) function_name: String,
    /// This block contains instructions that was inlined (function or macro) into the current sequence.
    pub(crate) is_inlined: bool,
    pub(crate) instructions: Vec<Instruction>,
    ///  - The `stepped_from` (left edge) identifies the address of the instruction immediately preceding this block.
    pub(crate) stepped_from: Option<u64>,
    ///  - The `steps_to` (right edge) identifies the address of the instruction immediately following this block:
    ///    - The address of the first instruction in the next block in the sequence, if there is one.
    ///    - The address of first instruction, after the instruction that called this sequence (return register value).
    pub(crate) steps_to: Option<u64>,
}

impl Block {
    /// The range of addresses that the block covers is 'inclusive' on both ends.
    pub(crate) fn included_addresses(&self) -> Option<RangeInclusive<u64>> {
        self.instructions
            .first()
            .map(|first| &first.address)
            .and_then(|first| self.instructions.last().map(|last| *first..=last.address))
    }

    /// Find the valid halt instruction location that is equal to, or greater than, the address.
    pub(crate) fn match_address(&self, address: u64) -> Option<&Instruction> {
        self.included_addresses().and_then(|included_addresses| {
            if included_addresses.contains(&address) {
                self.instructions.iter().find(|&location| {
                    location.role.is_halt_location() && location.address >= address
                })
            } else {
                None
            }
        })
    }

    /// Find the valid halt instruction location that that matches the `file`, `line` and `column`.
    /// If `column` is `None`, then the first instruction location that matches the `file` and `line` is returned.
    /// TODO: If there is a match, but it is not a valid halt location, then the next valid halt location is returned.
    pub(crate) fn match_location(
        &self,
        matching_file_index: Option<u64>,
        line: u64,
        column: Option<u64>,
    ) -> Option<&Instruction> {
        // Cycle through various degrees of matching, to find the most relevant source location.
        if let Some(supplied_column) = column {
            // Try an exact match.
            self.instructions
                .iter()
                .find(|&location| {
                    location.role.is_halt_location()
                        && matching_file_index == Some(location.file_index)
                        && NonZeroU64::new(line) == location.line
                        && ColumnType::from(supplied_column) == location.column
                })
                .or_else(|| {
                    // Try without a column specifier.
                    self.instructions.iter().find(|&location| {
                        location.role.is_halt_location()
                            && matching_file_index == Some(location.file_index)
                            && NonZeroU64::new(line) == location.line
                    })
                })
        } else {
            self.instructions.iter().find(|&location| {
                location.role.is_halt_location()
                    && matching_file_index == Some(location.file_index)
                    && NonZeroU64::new(line) == location.line
            })
        }
    }
}
