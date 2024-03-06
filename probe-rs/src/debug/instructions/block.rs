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
/// - To facilitate 'stepping', we also need to identify how blocks transition from one to the next,
///   and unlike inside a sequence, these are typically not sequential addresses. The `stepped_from` and `steps_to`
///   fields are used to identify the addresses of the instructions that are the left and right edges of the block.
///   The DWARF line program rows do not have enough information to identify branching instructions, and so we
///   cannot rely on the sequence of instructions in a line program sequence to identify the block boundaries.
///   To avoid having to interpret the Assembly instructions for every architecture, we use some basic heuristics
///   to identify block boundaries. Some of these can be inferred from the DWARF debug information, while others
///   can only be assessed using information about the stackframes in an unwinding context.
/// - The DWARF based heuristics used to identify block boundaries are as follows:
///   - The first block is the prologue block, and is identified by the `DW_LNS_set_prologue_end` attribute on the first
///   - first insruction after the prologue.
///   - The first block after the prologue, steps directly from the prologue block.
///   - Inlined code (functions or macros) always precede the instruction that called them. They are in their own block,
///     and will step to the calling instruction.
///   - If a function/sequence has multiple ranges, then the instructions in those ranges are assumed to be
///     divergent in some way.
///   - The remaining instructions are grouped into blocks containing the contiguous instructions belonging to the same
///     source file line.
/// - After applying the DWARF based heuristics, the remaining block boundaries are inferred from the stackframes when
///   they are available (target is halted and unwinding is possible).
/// - If after all this, we need to step from/to/into blocks with insufficient boundary information, then we resort to
///   the following strategy:
///   - Once the target is active and halted in the relevant sequence, then we can single step the processor,
///     until we reach a new block, a new sequence, and based on the result,
///     we can update the block boundaries. e.g. If after stepping the processor by one instruction,
///     we find ourselves in the prologue of a different function, then we know we have stepped `into`
///     a function call, and we can update the block boundaries (and stepping logic) accordingly.
///   - If the target is active and halted in a different sequence, e.g. during reset-and-halt, then
///     we can infer breakpoints based on the 'closest available line', or if that is not possible, we
///     inform the user that insufficient information is available to set a breakpoint at the requested location.
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
