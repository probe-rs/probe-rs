use super::{
    ColumnType, DebugError, DebugInfo, GimliReader, canonical_path_eq,
    unit_info::{self, UnitInfo},
};
use gimli::LineSequence;
use serde::Serialize;
use std::{
    fmt::{Debug, Formatter},
    num::NonZeroU64,
    ops::Range,
};
use typed_path::{TypedPath, TypedPathBuf};

/// A verified breakpoint represents an instruction address, and the source location that it corresponds to it,
/// for locations in the target binary that comply with the DWARF standard terminology for "recommended breakpoint location".
/// This typically refers to instructions that are not part of the prologue or epilogue, and are part of the user code,
/// or are the final instruction in a sequence, before the processor begins the epilogue code.
/// The `probe-rs` debugger uses this information to identify valid halt locations for breakpoints and stepping.
#[derive(Clone, Debug)]
pub struct VerifiedBreakpoint {
    /// The address in target memory, where the breakpoint can be set.
    pub address: u64,
    /// If the breakpoint request was for a specific source location, then this field will contain the resolved source location.
    pub source_location: SourceLocation,
}

impl VerifiedBreakpoint {
    /// Return the first valid breakpoint location of the statement that is greater than OR equal to `address`.
    /// e.g., if the `address` is the current program counter, then the return value will be the next valid halt address
    /// in the current sequence.
    pub(crate) fn for_address(
        debug_info: &DebugInfo,
        address: u64,
    ) -> Result<VerifiedBreakpoint, DebugError> {
        let instruction_sequence = InstructionSequence::from_address(debug_info, address)?;

        // Cycle through various degrees of matching, to find the most relevant source location.
        if let Some(verified_breakpoint) = match_address(&instruction_sequence, address, debug_info)
        {
            tracing::debug!(
                "Found valid breakpoint for address: {:#010x} : {verified_breakpoint:?}",
                &address
            );
            return Ok(verified_breakpoint);
        }
        // If we get here, we have not found a valid breakpoint location.
        let message = format!(
            "Could not identify a valid breakpoint for address: {address:#010x}. Please consider using instruction level stepping."
        );
        Err(DebugError::WarnAndContinue { message })
    }

    /// Identifying the breakpoint location for a specific location (path, line, column) is a bit more complex,
    /// compared to the `for_address()` method, due to a few factors:
    /// - The correct program instructions, may be in any of the compilation units of the current program.
    /// - The debug information may not contain data for the "specific source" location requested:
    ///   - DWARFv5 standard, section 6.2, allows omissions based on certain conditions. In this case,
    ///     we need to find the closest "relevant" source location that has valid debug information.
    ///   - The requested location may not be a valid source location, e.g. when the
    ///     debug information has been optimized away. In this case we will return an appropriate error.
    ///
    /// #### The logic used to find the "most relevant" source location is as follows:
    /// 1. Filter  [`UnitInfo`], by using [`gimli::LineProgramHeader`] to match units that include
    ///    the requested path.
    /// 2. For each matching compilation unit, get the [`gimli::LineProgram`] and
    ///    [`Vec<LineSequence>`][LineSequence].
    /// 3. Filter the [`Vec<LineSequence>`][LineSequence] entries to only include sequences that match the requested path.
    /// 3. Convert remaining [`LineSequence`], to [`InstructionSequence`].
    /// 4. Return the first [`InstructionSequence`] that contains the requested source location.
    ///    1. This may be an exact match on file/line/column, or,
    ///    2. Failing an exact match, a match on file/line only.
    ///    3. Failing that, a match on file only, where the line number is the "next" available instruction,
    ///       on the next available line of the specified file.
    pub(crate) fn for_source_location(
        debug_info: &DebugInfo,
        path: TypedPath,
        line: u64,
        column: Option<u64>,
    ) -> Result<Self, DebugError> {
        for program_unit in &debug_info.unit_infos {
            let Some(ref line_program) = program_unit.unit.line_program else {
                // Not all compilation units need to have debug line information, so we skip those.
                continue;
            };

            let mut num_files = line_program.header().file_names().len();

            // For DWARF version 5, the current compilation file is included in the file names, with index 0.
            //
            // For earlier versions, the current compilation file is not included in the file names, but index 0 still refers to it.
            // To get the correct number of files, we have to add 1 here.
            if program_unit.unit.header.version() <= 4 {
                num_files += 1;
            }

            // There can be multiple file indices which match, due to the inclusion of the current compilation file with index 0.
            //
            // At least for DWARF 4 there are cases where the current compilation file is also included in the file names with
            // a non-zero index.
            let matching_file_indices: Vec<_> = (0..num_files)
                .filter_map(|file_index| {
                    let file_index = file_index as u64;

                    debug_info
                        .get_path(&program_unit.unit, file_index)
                        .and_then(|combined_path: TypedPathBuf| {
                            if canonical_path_eq(path, combined_path.to_path()) {
                                tracing::debug!(
                                    "Found matching file index: {file_index} for path: {path}",
                                    file_index = file_index,
                                    path = path.display()
                                );
                                Some(file_index)
                            } else {
                                None
                            }
                        })
                })
                .collect();

            if matching_file_indices.is_empty() {
                continue;
            }

            let Ok((complete_line_program, line_sequences)) = line_program.clone().sequences()
            else {
                tracing::debug!("Failed to get line sequences for line program");
                continue;
            };

            for line_sequence in line_sequences {
                let instruction_sequence = InstructionSequence::from_line_sequence(
                    debug_info,
                    program_unit,
                    &complete_line_program,
                    &line_sequence,
                );

                for matching_file_index in &matching_file_indices {
                    // Cycle through various degrees of matching, to find the most relevant source location.
                    if let Some(verified_breakpoint) = match_file_line_column(
                        &instruction_sequence,
                        *matching_file_index,
                        line,
                        column,
                        debug_info,
                        program_unit,
                    ) {
                        return Ok(verified_breakpoint);
                    }

                    if let Some(verified_breakpoint) = match_file_line_first_available_column(
                        &instruction_sequence,
                        *matching_file_index,
                        line,
                        debug_info,
                        program_unit,
                    ) {
                        return Ok(verified_breakpoint);
                    }
                }
            }
        }
        // If we get here, we have not found a valid breakpoint location.
        Err(DebugError::Other(format!(
            "No valid breakpoint information found for file: {}, line: {line:?}, column: {column:?}",
            path.display()
        )))
    }
}

/// Find the valid halt instruction location that is equal to, or greater than, the address.
fn match_address(
    instruction_sequence: &InstructionSequence<'_>,
    address: u64,
    debug_info: &DebugInfo,
) -> Option<VerifiedBreakpoint> {
    if instruction_sequence.address_range.contains(&address) {
        let instruction_location =
            instruction_sequence
                .instructions
                .iter()
                .find(|instruction_location| {
                    instruction_location.instruction_type == InstructionType::HaltLocation
                        && instruction_location.address >= address
                })?;

        let source_location = SourceLocation::from_instruction_location(
            debug_info,
            instruction_sequence.program_unit,
            instruction_location,
        )?;

        Some(VerifiedBreakpoint {
            address: instruction_location.address,
            source_location,
        })
    } else {
        None
    }
}

/// Find the valid halt instruction location that matches the file, line and column.
fn match_file_line_column(
    instruction_sequence: &InstructionSequence<'_>,
    matching_file_index: u64,
    line: u64,
    column: Option<u64>,
    debug_info: &DebugInfo,
    program_unit: &UnitInfo,
) -> Option<VerifiedBreakpoint> {
    let instruction_location =
        instruction_sequence
            .instructions
            .iter()
            .find(|instruction_location| {
                instruction_location.instruction_type == InstructionType::HaltLocation
                    && matching_file_index == instruction_location.file_index
                    && NonZeroU64::new(line) == instruction_location.line
                    && column
                        .map(ColumnType::Column)
                        .is_some_and(|col| col == instruction_location.column)
            })?;

    let source_location =
        SourceLocation::from_instruction_location(debug_info, program_unit, instruction_location)?;

    Some(VerifiedBreakpoint {
        address: instruction_location.address,
        source_location,
    })
}

/// Find the first valid halt instruction location that matches the file and line, ignoring column.
fn match_file_line_first_available_column(
    instruction_sequence: &InstructionSequence<'_>,
    matching_file_index: u64,
    line: u64,
    debug_info: &DebugInfo,
    program_unit: &UnitInfo,
) -> Option<VerifiedBreakpoint> {
    let instruction_location =
        instruction_sequence
            .instructions
            .iter()
            .find(|instruction_location| {
                instruction_location.instruction_type == InstructionType::HaltLocation
                    && matching_file_index == instruction_location.file_index
                    && NonZeroU64::new(line) == instruction_location.line
            })?;

    let source_location =
        SourceLocation::from_instruction_location(debug_info, program_unit, instruction_location)?;

    Some(VerifiedBreakpoint {
        address: instruction_location.address,
        source_location,
    })
}

fn serialize_typed_path<S>(path: &TypedPathBuf, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&path.to_string_lossy())
}

/// A specific location in source code.
/// Each unique line, column, file and directory combination is a unique source location.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct SourceLocation {
    /// The path to the source file
    #[serde(serialize_with = "serialize_typed_path")]
    pub path: TypedPathBuf,
    /// The line number in the source file with zero based indexing.
    pub line: Option<u64>,
    /// The column number in the source file.
    pub column: Option<ColumnType>,
    /// The address of the source location.
    pub address: Option<u64>,
}

impl Debug for SourceLocation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{:?}:{:?}",
            self.path.to_path().display(),
            self.line,
            self.column
        )
    }
}

impl SourceLocation {
    /// Resolve debug information for a [`InstructionLocation`] and create a [`SourceLocation`].
    fn from_instruction_location(
        debug_info: &DebugInfo,
        program_unit: &unit_info::UnitInfo,
        instruction_location: &InstructionLocation,
    ) -> Option<SourceLocation> {
        debug_info
            .find_file_and_directory(&program_unit.unit, instruction_location.file_index)
            .map(|path| SourceLocation {
                line: instruction_location.line.map(std::num::NonZeroU64::get),
                column: Some(instruction_location.column),
                path,
                address: Some(instruction_location.address),
            })
    }

    /// Get the file name of the source file
    pub fn file_name(&self) -> Option<String> {
        self.path
            .file_name()
            .map(|name| String::from_utf8_lossy(name).to_string())
    }
}

/// Keep track of all the instruction locations required to satisfy the operations of [`SteppingMode`][s].
/// This is a list of target instructions, belonging to a [`gimli::LineSequence`],
/// and filters it to only user code instructions (no prologue code, and no non-statement instructions),
/// so that we are left only with what DWARF terms as "recommended breakpoint location".
///
/// [s]: crate::debug::debug_step::SteppingMode
struct InstructionSequence<'debug_info> {
    /// The `address_range.start` is the starting address of the program counter for which this sequence is valid,
    /// and allows us to identify target instruction locations where the program counter lies inside the prologue.
    /// The `address_range.end` is the first address that is not covered by this sequence within the line number program,
    /// and allows us to identify when stepping over a instruction location would result in leaving a sequence.
    /// - This is typically the instruction address of the first instruction in the next sequence,
    ///   which may also be the first instruction in a new function.
    address_range: Range<u64>,
    // NOTE: Use Vec as a container, because we will have relatively few statements per sequence, and we need to maintain the order.
    instructions: Vec<InstructionLocation>,
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
            writeln!(
                f,
                "\t{instruction_location:?} - {}",
                self.debug_info
                    .get_path(&self.program_unit.unit, instruction_location.file_index)
                    .map(|file_path| file_path.to_string_lossy().to_string())
                    .unwrap_or("<unknown file>".to_string())
            )?;
        }
        Ok(())
    }
}

impl<'debug_info> InstructionSequence<'debug_info> {
    /// Extract all the instruction locations, belonging to the active sequence (i.e. the sequence that contains the `address`).
    fn from_address(
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
            let message = "The specified source location does not have any line_program information available. Please consider using instruction level stepping.".to_string();
            return Err(DebugError::WarnAndContinue { message });
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
            let message = "The specified source location does not have any line information available. Please consider using instruction level stepping.".to_string();
            return Err(DebugError::WarnAndContinue { message });
        };
        let instruction_sequence = Self::from_line_sequence(
            debug_info,
            program_unit,
            &complete_line_program,
            line_sequence,
        );

        if instruction_sequence.len() == 0 {
            let message = "Could not find valid instruction locations for this address. Consider using instruction level stepping.".to_string();
            Err(DebugError::WarnAndContinue { message })
        } else {
            tracing::trace!(
                "Instruction location for pc={:#010x}\n{:?}",
                program_counter,
                instruction_sequence
            );
            Ok(instruction_sequence)
        }
    }

    /// Build [`InstructionSequence`] from a [`gimli::LineSequence`], with all the markers we need to determine valid halt locations.
    fn from_line_sequence(
        debug_info: &'debug_info DebugInfo,
        program_unit: &'debug_info UnitInfo,
        complete_line_program: &gimli::CompleteLineProgram<GimliReader>,
        line_sequence: &LineSequence<GimliReader>,
    ) -> Self {
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
                && let Some(prev_row) = previous_row
                && (row.end_sequence()
                    || (row.is_stmt()
                        && (row.file_index() == prev_row.file_index()
                            && (row.line() != prev_row.line() || row.line().is_none()))))
            {
                prologue_completed = true;
            }

            if !prologue_completed {
                log_row_eval(line_sequence, row, "  inside prologue>");
            } else {
                log_row_eval(line_sequence, row, "  after prologue>");
            }

            // The end of the sequence is not a valid halt location,
            // nor is it a valid instruction in the current sequence.
            if row.end_sequence() {
                break;
            }

            instruction_sequence.add(prologue_completed, row, previous_row.as_ref());
            previous_row = Some(*row);
        }
        instruction_sequence
    }

    /// Add a instruction location to the list.
    fn add(
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

        let instruction_location = InstructionLocation {
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

        self.instructions.push(instruction_location);
    }

    /// Get the number of instruction locations in the list.
    fn len(&self) -> usize {
        self.instructions.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// The type of instruction, as defined by [`gimli::LineRow`] attributes and relative position in the sequence.
enum InstructionType {
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
/// - A [`InstructionLocation`] filters and maps [`gimli::LineRow`] entries to be used for determining valid halt points.
///   - Each [`InstructionLocation`] maps to a single machine instruction on target.
///   - For establishing valid halt locations (breakpoint or stepping), we are only interested,
///     in the [`InstructionLocation`]'s that represent DWARF defined `statements`,
///     which are not part of the prologue or epilogue.
/// - A line of code in a source file may contain multiple instruction locations, in which case
///   a new [`InstructionLocation`] with unique `column` is created.
/// - A [`InstructionSequence`] is a series of contiguous [`InstructionLocation`]'s.
struct InstructionLocation {
    address: u64,
    file_index: u64,
    line: Option<NonZeroU64>,
    column: ColumnType,
    instruction_type: InstructionType,
}

impl Debug for InstructionLocation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Instruction @ {:010x}, on line={:04}  col={:05}  f={:02}, type={:?}",
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

/// Helper function to avoid code duplication when logging of information during row evaluation.
fn log_row_eval(
    active_sequence: &LineSequence<super::GimliReader>,
    row: &gimli::LineRow,
    status: &str,
) {
    tracing::trace!(
        "Sequence: line={:04} col={:05} f={:02} stmt={:5} ep={:5} es={:5} eb={:5} : {:#010X}<={:#010X}<{:#010X} : {}",
        match row.line() {
            Some(line) => line.get(),
            None => 0,
        },
        match row.column() {
            gimli::ColumnType::LeftEdge => 0,
            gimli::ColumnType::Column(column) => column.get(),
        },
        row.file_index(),
        row.is_stmt(),
        row.prologue_end(),
        row.end_sequence(),
        row.epilogue_begin(),
        active_sequence.start,
        row.address(),
        active_sequence.end,
        status
    );
}
