use super::{
    unit_info::{self, UnitInfo},
    ColumnType, DebugError, DebugInfo,
};
use gimli::LineSequence;
use std::{
    fmt::{Debug, Formatter},
    num::NonZeroU64,
    ops::Range,
    path::PathBuf,
};
use typed_path::TypedPathBuf;

/// Capture the required information when a breakpoint is set based on a requested source location.
/// It is possible that the requested source location cannot be resolved to a valid instruction address,
/// in which case the first 'valid' instruction address will be used, and the source location will be
/// updated to reflect the actual source location, not the requested source location.
#[derive(Clone, Debug)]
pub struct VerifiedBreakpoint {
    /// The address in target memory, where the breakpoint can be set.
    pub address: u64,
    /// If the breakpoint request was for a specific source location, then this field will contain the resolved source location.
    pub source_location: SourceLocation,
}

fn serialize_typed_path<S>(path: &Option<TypedPathBuf>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match path {
        Some(path) => serializer.serialize_str(&path.to_string_lossy()),
        None => serializer.serialize_none(),
    }
}

/// A specific location in source code.
/// Each unique line, column, file and directory combination is a unique source location.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SourceLocation {
    /// The line number in the source file with zero based indexing.
    pub line: Option<u64>,
    /// The column number in the source file with zero based indexing.
    pub column: Option<ColumnType>,
    /// The file name of the source file.
    pub file: Option<String>,
    /// The directory of the source file.
    #[serde(serialize_with = "serialize_typed_path")]
    pub directory: Option<TypedPathBuf>,
}

impl SourceLocation {
    /// Resolve debug information for a [`InstructionLocation`] and create a [`SourceLocation`].
    pub(crate) fn from_instruction_location(
        debug_info: &DebugInfo,
        program_unit: &unit_info::UnitInfo,
        instruction_location: &InstructionLocation,
    ) -> Option<SourceLocation> {
        let line_program = program_unit.unit.line_program.as_ref()?;
        let file_entry = line_program
            .header()
            .file(instruction_location.file_index)?;
        debug_info
            .find_file_and_directory(&program_unit.unit, line_program.header(), file_entry)
            .map(|(file, directory)| SourceLocation {
                line: instruction_location.line.map(std::num::NonZeroU64::get),
                column: Some(instruction_location.column),
                file,
                directory,
            })
    }

    /// The full path of the source file, combining the `directory` and `file` fields.
    /// If the path does not resolve to an existing file, an error is returned.
    pub fn combined_path(&self) -> Result<PathBuf, DebugError> {
        let combined_path = self.combined_typed_path();

        if let Some(native_path) = combined_path.and_then(|p| PathBuf::try_from(p).ok()) {
            if native_path.exists() {
                return Ok(native_path);
            }
        }

        Err(DebugError::Other(anyhow::anyhow!(
            "Unable to find source file for directory {:?} and file {:?}",
            self.directory,
            self.file
        )))
    }

    /// Get the full path of the source file
    pub fn combined_typed_path(&self) -> Option<TypedPathBuf> {
        let combined_path = self
            .directory
            .as_ref()
            .and_then(|dir| self.file.as_ref().map(|file| dir.join(file)));

        combined_path
    }
}

/// Keep track of all the instruction locations required to satisfy the operations of [`SteppingMode`].
/// This is a list of target instructions, belonging to a [`gimli::LineSequence`],
/// and filters it to only user code instructions (no prologue code, and no non-statement instructions),
/// so that we are left only with what DWARF terms as "recommended breakpoint location".
pub struct InstructionSequence<'debug_info> {
    /// The `address_range.start` is the starting address of the program counter for which this sequence is valid,
    /// and allows us to identify target instruction locations where the program counter lies inside the prologue.
    /// The `address_range.end` is the first address that is not covered by this sequence within the line number program,
    /// and allows us to identify when stepping over a instruction location would result in leaving a sequence.
    pub(crate) address_range: Range<u64>,
    // NOTE: Use Vec as a container, because we will have relatively few statements per sequence, and we need to maintain the order.
    pub(crate) instructions: Vec<InstructionLocation>,
    // The following private fields are required to resolve the source location information for
    // each instruction location.
    debug_info: &'debug_info DebugInfo,
    program_unit: &'debug_info UnitInfo,
}

impl Debug for InstructionSequence<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "Instruction Sequence with address range: {:#010x} - {:#010x}",
            self.address_range.start, self.address_range.end
        )?;
        for instruction_location in &self.instructions {
            writeln!(f, "\t{instruction_location:?}")?;
        }
        Ok(())
    }
}

impl<'debug_info> InstructionSequence<'debug_info> {
    /// Extract all the instruction locations, belonging to the active sequence (i.e. the sequence that contains the `program_counter`).
    pub(crate) fn for_address(
        debug_info: &'debug_info DebugInfo,
        program_counter: u64,
    ) -> Result<Self, DebugError> {
        let program_unit = debug_info.compile_unit_info(program_counter)?;
        let (offset, address_size) = if let Some(line_program) =
            program_unit.unit.line_program.clone()
        {
            (
                line_program.header().offset(),
                line_program.header().address_size(),
            )
        } else {
            return Err(DebugError::IncompleteDebugInfo{
                        message: "The specified source location does not have any line_program information available. Please consider using instruction level stepping.".to_string(),
                        pc_at_error: program_counter,
                    });
        };

        // Get the sequences of rows from the CompleteLineProgram at the given program_counter.
        let incomplete_line_program =
            debug_info
                .debug_line_section
                .program(offset, address_size, None, None)?;
        let (complete_line_program, line_sequences) = incomplete_line_program.sequences()?;

        // Get the sequence of rows that belongs to the program_counter.
        let Some(line_sequence) = line_sequences.iter().find(|line_sequence| {
            line_sequence.start <= program_counter && program_counter < line_sequence.end
        }) else {
            return Err(DebugError::IncompleteDebugInfo{
                        message: "The specified source location does not have any line information available. Please consider using instruction level stepping.".to_string(),
                        pc_at_error: program_counter,
                    });
        };
        let program_language = program_unit.get_language();
        let mut sequence_rows = complete_line_program.resume_from(line_sequence);

        // We have enough information to create the InstructionSequence.
        let mut instruction_sequence = InstructionSequence {
            address_range: line_sequence.start..line_sequence.end,
            instructions: Vec::new(),
            debug_info,
            program_unit,
        };
        let mut prologue_completed = false;
        let mut previous_row: Option<gimli::LineRow> = None;
        while let Ok(Some((_, row))) = sequence_rows.next_row() {
            // Don't do anything until we are at least at the prologue_end() of a function.
            if row.prologue_end() {
                prologue_completed = true;
            }

            // For GNU C, it is known that the `DW_LNS_set_prologue_end` is not set, so we employ the same heuristic as GDB to determine when the prologue is complete.
            // For other C compilers in the C99/11/17 standard, they will either set the `DW_LNS_set_prologue_end` or they will trigger this heuristic also.
            // See https://gcc.gnu.org/legacy-ml/gcc-patches/2011-03/msg02106.html
            if !prologue_completed
                && matches!(
                    program_language,
                    gimli::DW_LANG_C99 | gimli::DW_LANG_C11 | gimli::DW_LANG_C17
                )
            {
                if let Some(prev_row) = previous_row {
                    if row.end_sequence()
                        || (row.is_stmt()
                            && (row.file_index() == prev_row.file_index()
                                && (row.line() != prev_row.line() || row.line().is_none())))
                    {
                        prologue_completed = true;
                    }
                }
            }

            if !prologue_completed {
                log_row_eval(line_sequence, program_counter, row, "  inside prologue>");
                previous_row = Some(*row);
                continue;
            } else {
                log_row_eval(line_sequence, program_counter, row, "  after prologue>");
            }

            // The end of the sequence is not a valid halt location,
            // nor is it a valid instruction in the current sequence.
            if row.end_sequence() {
                // Mark the previous instruction as the last valid instruction in the sequence.
                if let Some(previous_instruction) = instruction_sequence.instructions.last_mut() {
                    previous_instruction.is_sequence_exit = true;
                }
                break;
            }

            if row.is_stmt() {
                instruction_sequence.add(row, previous_row.as_ref());
            }
        }

        if instruction_sequence.len() == 0 {
            Err(DebugError::IncompleteDebugInfo{
                message: "Could not find valid instruction locations for this address. Consider using instruction level stepping.".to_string(),
                pc_at_error: program_counter,
            })
        } else {
            tracing::trace!(
                "Instruction location for pc={:#010x}\n{:?}",
                program_counter,
                instruction_sequence
            );
            Ok(instruction_sequence)
        }
    }

    /// Identifying the instruction locations for a specific location (path, line, colunmn) is a bit more complex,
    /// compared to the `for_address()` method, due to a few factors:
    /// - We need to find the correct program instructions, which may be in any of the compilation
    /// units of the current program.
    /// - The debug information may not contain data for the "requested source location", e.g.
    ///   - DWARFv5 standard, section 6.2, allows omissions based on certain conditions. In this case,
    ///    we need to find the closest "relevant" source location that has valid debug information.
    ///   - The requested location may not be a valid source location, e.g. when the
    ///    debug information has been optimized away. In this case we will return an appropriate error.
    #[allow(dead_code)] // temporary, while this PR is a WIP
    pub(crate) fn for_source_location(
        _path: &TypedPathBuf,
        _line: u64,
        _column: Option<u64>,
    ) -> Result<Self, DebugError> {
        Err(DebugError::Other(anyhow::anyhow!("TODO")))
    }

    /// Add a instruction location to the list.
    pub(crate) fn add(&mut self, row: &gimli::LineRow, previous_row: Option<&gimli::LineRow>) {
        let mut instruction_location = InstructionLocation::from(row);
        if let Some(prev_row) = previous_row {
            if row.line().is_none()
                && prev_row.line().is_some()
                && row.file_index() == prev_row.file_index()
                && prev_row.column() == row.column()
            {
                // Workaround the line number issue (if recorded as 0 in the DWARF, then gimli reports it as None).
                // For debug purposes, it makes more sense to be the same as the previous line, which almost always
                // has the same file index and column value.
                // This prevents the debugger from jumping to the top of the file unexpectedly.
                instruction_location.line = prev_row.line();
            }
        }
        self.instructions.push(instruction_location);
    }

    /// Get the number of instruction locations in the list.
    pub(crate) fn len(&self) -> usize {
        self.instructions.len()
    }

    /// Return the first valid breakpoint location of the statement that is greater than OR equal to `address`.
    /// e.g., if the `address` is the current program counter, and the return value will be the next valid halt address
    /// in the current sequence. A result of `None` indicates that the next valid halt address is outside the current sequence.
    pub(crate) fn get_first_breakpoint(
        &self,
        address: u64,
    ) -> Result<VerifiedBreakpoint, DebugError> {
        // Note: The `address_range` captures address range the prologue, in addition to the valid instructions in the sequence.
        if self.address_range.contains(&address) {
            if let Some(valid_breakpoint) = self
                .instructions
                .iter()
                .find(|instruction_location| instruction_location.address >= address)
                .and_then(|instruction_location| {
                    SourceLocation::from_instruction_location(
                        self.debug_info,
                        self.program_unit,
                        instruction_location,
                    )
                    .map(|source_location| VerifiedBreakpoint {
                        address: instruction_location.address,
                        source_location,
                    })
                })
            {
                tracing::debug!(
                    "Found valid breakpoint for address: {:#010x} : {valid_breakpoint:?}",
                    &address
                );
                return Ok(valid_breakpoint);
            }
        }
        Err(DebugError::IncompleteDebugInfo{
            message: "Could not determine valid halt locations for this request. Please consider using instruction level stepping.".to_string(),
            pc_at_error: address,
        })
    }
}

#[derive(Clone)]
/// - A [`InstructionLocation`] filters and maps [`gimli::LineRow`] entries to be used for determining valid halt points.
///   - Each [`InstructionLocation`] maps to a single machine instruction on target.
///   - For establishing valid halt locations (breakpoint or stepping), we are only interested,
///     in the [`InstructionLocation`]'s that represent DWARF defined `statements`,
///     which are not part of the prologue or epilogue.
/// - A line of code in a source file may contain multiple instruction locations, in which case
///     a new [`InstructionLocation`] with unique `column` is created.
/// - A [`InstructionSequence`] is a series of contiguous [`InstructionLocation`]'s.
pub(crate) struct InstructionLocation {
    pub(crate) address: u64,
    pub(crate) file_index: u64,
    pub(crate) line: Option<NonZeroU64>,
    pub(crate) column: ColumnType,
    /// Indicates that this instruction location is either the beginning of an epilogue,
    /// or the last valid instruction in the sequence.
    pub(crate) is_sequence_exit: bool,
}

impl Debug for InstructionLocation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Instruction @ {:010x}, on line={:04}  col={:05}  f={:02}, is_sequence_exit={:?}",
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
            &self.is_sequence_exit,
        )?;
        Ok(())
    }
}

impl From<&gimli::LineRow> for InstructionLocation {
    fn from(line_row: &gimli::LineRow) -> Self {
        InstructionLocation {
            address: line_row.address(),
            file_index: line_row.file_index(),
            line: line_row.line(),
            column: line_row.column().into(),
            is_sequence_exit: line_row.epilogue_begin(),
        }
    }
}

/// Helper function to avoid code duplication when logging of information during row evaluation.
fn log_row_eval(
    active_sequence: &LineSequence<super::GimliReader>,
    pc: u64,
    row: &gimli::LineRow,
    status: &str,
) {
    tracing::trace!("Sequence: line={:04} col={:05} f={:02} addr={:#010X} stmt={:5} ep={:5} es={:5} eb={:5} : {:#010X}<={:#010X}<{:#010X} : {}",
        match row.line() {
            Some(line) => line.get(),
            None => 0,
        },
        match row.column() {
            gimli::ColumnType::LeftEdge => 0,
            gimli::ColumnType::Column(column) => column.get(),
        },
        row.file_index(),
        row.address(),
        row.is_stmt(),
        row.prologue_end(),
        row.end_sequence(),
        row.epilogue_begin(),
        active_sequence.start,
        pc,
        active_sequence.end,
        status);
}
