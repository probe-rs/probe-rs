use super::{
    super::ColumnType,
    instruction::{Instruction, InstructionType},
};
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
pub(crate) struct Block {
    /// The range of addresses that the block covers is 'inclusive' on both ends.
    pub(crate) included_addresses: RangeInclusive<u64>,
    pub(crate) instructions: Vec<Instruction>,
}

impl Block {
    /// Find the valid halt instruction location that is equal to, or greater than, the address.
    pub(crate) fn match_address(&self, address: u64) -> Option<&Instruction> {
        if self.included_addresses.contains(&address) {
            self.instructions.iter().find(|&location| {
                location.instruction_type == InstructionType::HaltLocation
                    && location.address >= address
            })
        } else {
            None
        }
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
                    location.instruction_type == InstructionType::HaltLocation
                        && matching_file_index == Some(location.file_index)
                        && NonZeroU64::new(line) == location.line
                        && ColumnType::from(supplied_column) == location.column
                })
                .or_else(|| {
                    // Try without a column specifier.
                    self.instructions.iter().find(|&location| {
                        location.instruction_type == InstructionType::HaltLocation
                            && matching_file_index == Some(location.file_index)
                            && NonZeroU64::new(line) == location.line
                    })
                })
        } else {
            self.instructions.iter().find(|&location| {
                location.instruction_type == InstructionType::HaltLocation
                    && matching_file_index == Some(location.file_index)
                    && NonZeroU64::new(line) == location.line
            })
        }
    }

    /// Add a instruction locations to the list.
    pub(crate) fn add(
        &mut self,
        prologue_completed: bool,
        row: &gimli::LineRow,
        previous_row: Option<&gimli::LineRow>,
    ) {
        // Workaround the line number issue (if recorded as 0 in the DWARF, then gimli reports it as None).
        // For debug purposes, it makes more sense to be the same as the previous line, which almost always
        // has the same file index and column value.
        // This prevents the debugger from jumping to the top of the file unexpectedly.
        let mut instruction_line = row.line();
        if let Some(prev_row) = previous_row
            && row.line().is_none()
            && prev_row.line().is_some()
            && row.file_index() == prev_row.file_index()
            && prev_row.column() == row.column()
        {
            instruction_line = prev_row.line();
        }

        let instruction = Instruction {
            address: row.address(),
            file_index: row.file_index(),
            line: instruction_line,
            column: row.column().into(),
            instruction_type: if !prologue_completed {
                InstructionType::Prologue
            } else if row.epilogue_begin() || row.is_stmt() {
                InstructionType::HaltLocation
            } else {
                InstructionType::Unspecified
            },
        };
        self.included_addresses = *self.included_addresses.start()..=row.address();
        self.instructions.push(instruction);
    }
}
