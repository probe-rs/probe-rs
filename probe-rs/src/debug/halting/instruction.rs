use super::super::ColumnType;
use std::{
    fmt::{Debug, Formatter},
    num::NonZeroU64,
};

#[derive(Debug, Copy, Clone, PartialEq)]
/// The role of instruction, as defined by [`gimli::LineRow`] attributes and relative position in the sequence.
pub(crate) enum InstructionRole {
    /// We need to keep track of source lines that signal function signatures.
    PrologueHaltPoint,
    /// In the prologue, but not a valid haltpoint.
    PrologueOther,
    /// An instruction where we can set a breakpoint and expect the processor to halt.
    HaltPoint,
    /// The last instruction before a function exits. See DWARF's Section 6.2.2 `epilogue_begin`.
    /// This will always be the last instruction in a `Block`, but provides more meaning in the context of
    /// a `Block`'s interaction in a `Sequence`.
    EpilogueBegin,
    /// Any other instruction that is not part of the prologue or epilogue, and is not a haltpoint.
    /// We keep track of these for stepping purposes, so that we can identify adjacent haltpoints.
    Other,
}

impl InstructionRole {
    /// Returns `true` if the instruction is a valid halt location,
    /// described by DWARF as a "recommended breakpoint location",
    pub(crate) fn is_halt_location(&self) -> bool {
        matches!(
            self,
            InstructionRole::PrologueHaltPoint
                | InstructionRole::HaltPoint
                | InstructionRole::EpilogueBegin
        )
    }
}

#[derive(Copy, Clone)]
/// - A [`Instruction`] filters and maps [`gimli::LineRow`] entries to be used for determining valid halt points.
///   - Each [`Instruction`] maps to a single machine instruction on target.
///   - For establishing valid halt locations (breakpoint or stepping), we are only interested,
///     in the [`Instruction`]'s that represent DWARF defined `statements`,
///     which are not part of the prologue or epilogue.
/// - A line of code in a source file may contain multiple instruction locations, in which case
///     a new [`Instruction`] with unique `column` is created.
/// - A [`Sequence`] is a series of contiguous [`Instruction`]'s.
pub(crate) struct Instruction {
    pub(crate) address: u64,
    pub(crate) file_index: u64,
    pub(crate) line: Option<NonZeroU64>,
    pub(crate) column: ColumnType,
    pub(crate) role: InstructionRole,
}

impl Instruction {
    /// Build a [`Instruction`] using [`gimli::LineRow`] information.
    pub(crate) fn from_line_row(
        prologue_completed: bool,
        row: &gimli::LineRow,
        previous_row: Option<&gimli::LineRow>,
    ) -> Self {
        // Workaround the line number issue (if recorded as 0 in the DWARF, then gimli reports it as None).
        // For debug purposes, it makes more sense to be the same as the previous line, which almost always
        // has the same file index and column value.
        // This prevents the debugger from jumping to the top of the file unexpectedly.
        let mut instruction_line = row.line();
        if let Some(prev_row) = previous_row {
            if row.line().is_none()
                && prev_row.line().is_some()
                && row.file_index() == prev_row.file_index()
                && prev_row.column() == row.column()
            {
                instruction_line = prev_row.line();
            }
        }

        Instruction {
            address: row.address(),
            file_index: row.file_index(),
            line: instruction_line,
            column: row.column().into(),
            role: if !prologue_completed {
                if row.is_stmt() {
                    InstructionRole::PrologueHaltPoint
                } else {
                    InstructionRole::PrologueOther
                }
            } else if row.is_stmt() {
                // This type may be later changed during further processing.
                InstructionRole::HaltPoint
            } else if row.epilogue_begin() {
                InstructionRole::EpilogueBegin
            } else {
                InstructionRole::Other
            },
        }
    }
}

impl Debug for Instruction {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:#010x}, line={:04}  col={:05}  f={:02}, type={:?}",
            &self.address,
            match &self.line {
                Some(line) => line.get(),
                None => 0,
            },
            match &self.column {
                ColumnType::LeftEdge => 0,
                ColumnType::Column(column) => column.to_owned(),
            },
            &self.file_index,
            &self.role,
        )?;
        Ok(())
    }
}
