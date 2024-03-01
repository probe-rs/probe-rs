use super::super::ColumnType;
use std::{
    fmt::{Debug, Formatter},
    num::NonZeroU64,
};

#[derive(Debug, Clone, Copy, PartialEq)]
/// The type of instruction, as defined by [`gimli::LineRow`] attributes and relative position in the sequence.
pub(crate) enum InstructionType {
    /// We need to keep track of source lines that signal function signatures,
    /// even if their program lines are not valid halt locations.
    Prologue,
    /// DWARF defined "recommended breakpoint location",
    /// typically marked with `is_stmt` or `epilogue_begin`.
    HaltLocation,
    /// Any other instruction that is not part of the prologue or epilogue, and is not a statement,
    /// is considered to be an unspecified instruction type.
    Unspecified,
}

#[derive(Clone, Copy)]
/// - A [`Instruction`] filters and maps [`gimli::LineRow`] entries to be used for determining valid halt points.
///   - Each [`Instruction`] maps to a single machine instruction on target.
///   - For establishing valid halt locations (breakpoint or stepping), we are only interested,
///     in the [`Instruction`]'s that represent DWARF defined `statements`,
///     which are not part of the prologue or epilogue.
/// - A line of code in a source file may contain multiple instruction locations, in which case
///   a new [`Instruction`] with unique `column` is created.
/// - A [`Sequence`] is a series of contiguous [`Instruction`]'s.
pub(crate) struct Instruction {
    pub(crate) address: u64,
    pub(crate) file_index: u64,
    pub(crate) line: Option<NonZeroU64>,
    pub(crate) column: ColumnType,
    pub(crate) instruction_type: InstructionType,
}

impl Instruction {}

impl Debug for Instruction {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:010x}, line={:04}  col={:05}  f={:02}, type={:?}",
            self.address,
            match self.line {
                Some(line) => line.get(),
                None => 0,
            },
            match self.column {
                ColumnType::LeftEdge => 0,
                ColumnType::Column(column) => column,
            },
            self.file_index,
            self.instruction_type,
        )
    }
}
