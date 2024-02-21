use super::{unit_info::UnitInfo, ColumnType, DebugError, DebugInfo};
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
    /// The address in target memory, where the breakpoint was set.
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
/// Each unique line, column, file and directory combination is a unique source location,
/// and maps to a contiguous and monotonic range of machine instructions (i.e. a sequence of instructions).
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
    /// The address of the first instruction associated with the source code
    pub low_pc: Option<u32>,
    /// The address of the first location past the last instruction associated with the source code
    pub high_pc: Option<u32>,
}

impl SourceLocation {
    /// Resolve debug information for a [`SourceStatement`] and create a [`SourceLocation`].
    pub(crate) fn from_source_statement(
        debug_info: &DebugInfo,
        program_unit: &super::unit_info::UnitInfo,
        line_program: &gimli::IncompleteLineProgram<
            gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>,
            usize,
        >,
        file_entry: &gimli::FileEntry<
            gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>,
            usize,
        >,
        instruction_location: super::source_instructions::InstructionLocation,
    ) -> Option<SourceLocation> {
        debug_info
            .find_file_and_directory(&program_unit.unit, line_program.header(), file_entry)
            .map(|(file, directory)| SourceLocation {
                line: instruction_location.line.map(std::num::NonZeroU64::get),
                column: Some(instruction_location.column),
                file,
                directory,
                low_pc: Some(instruction_location.low_pc() as u32),
                high_pc: Some(instruction_location.instruction_range.end as u32),
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

/// Keep track of all the source statements required to satisfy the operations of [`SteppingMode`].
/// This is a list of target instructions, belonging to a [`gimli::LineSequence`],
/// and filters it to only user code instructions (no prologue code, and no non-statement instructions),
/// so that we are left only with valid halt locations.
pub struct InstructionSequence {
    // NOTE: Use Vec as a container, because we will have relatively few statements per sequence, and we need to maintain the order.
    pub(crate) statements: Vec<InstructionLocation>,
}

impl Debug for InstructionSequence {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        for statement in &self.statements {
            writeln!(f, "{statement:?}")?;
        }
        Ok(())
    }
}

impl InstructionSequence {
    /// Extract all the source statements, belonging to the active sequence (i.e. the sequence that contains the `program_counter`).
    pub(crate) fn for_address(
        debug_info: &DebugInfo,
        program_counter: u64,
    ) -> Result<Self, DebugError> {
        let mut instruction_sequence = InstructionSequence {
            statements: Vec::new(),
        };
        let source_sequence = get_program_and_sequence_for_pc(debug_info, program_counter)?;
        let mut sequence_rows = source_sequence
            .complete_line_program
            .resume_from(&source_sequence.line_sequence);
        let program_language = source_sequence.program_unit.get_language();
        let mut prologue_completed = false;
        let mut instruction_location: Option<InstructionLocation> = None;
        while let Ok(Some((_, row))) = sequence_rows.next_row() {
            if let Some(source_row) = instruction_location.as_mut() {
                if source_row.line.is_none()
                    && row.line().is_some()
                    && row.file_index() == source_row.file_index
                    && source_row.column == row.column().into()
                {
                    // Workaround the line number issue (if recorded as 0 in the DWARF, then gimli reports it as None).
                    // For debug purposes, it makes more sense to be the same as the previous line.
                    // This prevents the debugger from jumping to the top of the file unexpectedly.
                    source_row.line = row.line();
                }
            } else {
                // Start tracking the source statement using this row.
                instruction_location = Some(InstructionLocation::from(row));
            }

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
                if let Some(source_row) = instruction_location.as_mut() {
                    if row.end_sequence()
                        || (row.is_stmt()
                            && (row.file_index() == source_row.file_index
                                && (row.line() != source_row.line || row.line().is_none())))
                    {
                        prologue_completed = true;
                    }
                }
            }

            if !prologue_completed {
                log_row_eval(
                    &source_sequence.line_sequence,
                    program_counter,
                    row,
                    "  inside prologue>",
                );
                continue;
            } else {
                log_row_eval(
                    &source_sequence.line_sequence,
                    program_counter,
                    row,
                    "  after prologue>",
                );
            }

            // Notes about the process of building the source statement:
            // 1. Start a new (and close off the previous) source statement, when we encounter end of sequence OR change of file/line/column.
            // 2. The starting range of the first source statement will always be greater than or equal to the program_counter.
            // 3. The values in the `instruction_location` are only updated before we exit the current iteration of the loop,
            // so that we can retroactively close off and store the source statement that belongs to previous `rows`.
            // 4. The debug_info sometimes has a `None` value for the `row.line` that was started in the previous row, in which case we need to carry the previous row `line` number forward. GDB ignores this fact, and it shows up during debug as stepping to the top of the file (line 0) unexpectedly.

            if let Some(source_row) = instruction_location.as_mut() {
                // Update the instruction_range.end value.
                source_row.instruction_range = source_row.low_pc()..row.address();

                if row.end_sequence()
                    || (row.is_stmt() && row.address() > source_row.low_pc())
                    || !(row.file_index() == source_row.file_index
                        && (row.line() == source_row.line || row.line().is_none())
                        && source_row.column == row.column().into())
                {
                    if source_row.low_pc() >= program_counter {
                        // We need to close off the "current" source statement and add it to the list.
                        source_row.statement_range =
                            program_counter..source_sequence.line_sequence.end;
                        instruction_sequence.add(source_row.clone());
                    }

                    if row.end_sequence() {
                        // If we hit the end of the sequence, we can get out of here.
                        break;
                    }
                    instruction_location = Some(InstructionLocation::from(row));
                } else if row.address() == program_counter {
                    // If we encounter the program_counter after the prologue, then we need to use this address as the low_pc, or else we run the risk of setting a breakpoint before the current program counter.
                    source_row.instruction_range = row.address()..row.address();
                }
            }
        }

        if instruction_sequence.len() == 0 {
            Err(DebugError::IncompleteDebugInfo{
                message: "Could not find valid source statements for this address. Consider using instruction level stepping.".to_string(),
                pc_at_error: program_counter,
            })
        } else {
            tracing::trace!(
                "Source statements for pc={:#010x}\n{:?}",
                program_counter,
                instruction_sequence
            );
            Ok(instruction_sequence)
        }
    }

    /// Identifying the source statements for a specific location (path, line, colunmn) is a bit more complex,
    /// compared to the `for_address()` method, due to a few factors:
    /// - We need to find the correct program instructions, which may be in any of the compilation
    /// units of the current program.
    /// - The debug information may not contain data for the "requested source location", e.g.
    ///   - DWARFv5 standard, section 6.2, allows omissions based on certain conditions. In this case,
    ///    we need to find the closest "relevant" source location that has valid debug information.
    ///   - The requested location may not be a valid source location, e.g. when the
    ///    debug information has been optimized away. In this case we will return an appropriate error.

    pub(crate) fn for_source(
        path: &TypedPathBuf,
        line: u64,
        column: Option<u64>,
    ) -> Result<Self, DebugError> {
        Err(DebugError::Other(anyhow::anyhow!("TODO")))
    }

    /// Add a new source statement to the list.
    pub(crate) fn add(&mut self, statement: InstructionLocation) {
        self.statements.push(statement);
    }

    /// Get the number of source statements in the list.
    pub(crate) fn len(&self) -> usize {
        self.statements.len()
    }
}

/// Uniquely identifies a sequence of instructions in a line program.
struct ProgramLineSequence<'a> {
    program_unit: &'a UnitInfo,
    complete_line_program: gimli::CompleteLineProgram<
        gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>,
        usize,
    >,
    line_sequence: gimli::LineSequence<gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>>,
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
    pub(crate) file_index: u64,
    pub(crate) line: Option<NonZeroU64>,
    pub(crate) column: ColumnType,
    /// The range of instruction addresses associated with a source statement.
    /// The `instruction_range.start` is the address of the first instruction which is greater than,
    ///  or equal to the program_counter and not inside the prologue.
    /// The `instruction_range.end` is the address of the row of the next the non-contiguous sequence,
    ///  i.e. not part of this statement.
    pub(crate) instruction_range: Range<u64>,
    /// The `statement_range.start` is the starting address of the program counter for which this sequence is valid,
    /// and allows us to identify target source statements where the program counter lies inside the prologue.
    /// The `statement_range.end` is the address of the first byte after the end of a sequence,
    /// and allows us to identify when stepping over a source statement would result in leaving a sequence.
    pub(crate) statement_range: Range<u64>,
}

impl Debug for InstructionLocation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "\tStatement on line={:04}  col={:05}  f={:02}, Range: {:#010x}-{:#010x} --> Sequence Range: {:#010x}-{:#010x}",
            match &self.line {
                Some(line) => line.get(),
                None => 0,
            },
            match &self.column {
                ColumnType::LeftEdge => 0,
                ColumnType::Column(column) => column.to_owned(),
            },
            &self.file_index,
            &self.instruction_range.start,
            &self.instruction_range.end,
            &self.statement_range.start,
            &self.statement_range.end,
        )?;
        Ok(())
    }
}

impl InstructionLocation {
    /// Return the first valid halt address of the statement that is greater than or equal to `address`.
    pub(crate) fn get_first_halt_address(&self, address: u64) -> Option<u64> {
        if self.instruction_range.start == address
            || (self.statement_range.start..self.instruction_range.end).contains(&address)
        {
            Some(self.low_pc())
        } else {
            None
        }
    }

    /// Get the low_pc of this instruction_location.
    pub(crate) fn low_pc(&self) -> u64 {
        self.instruction_range.start
    }
}

impl From<&gimli::LineRow> for InstructionLocation {
    fn from(line_row: &gimli::LineRow) -> Self {
        InstructionLocation {
            file_index: line_row.file_index(),
            line: line_row.line(),
            column: line_row.column().into(),
            instruction_range: line_row.address()..line_row.address(),
            statement_range: line_row.address()..line_row.address(),
        }
    }
}

/// Resolve the relevant program and line-sequence row data for the given program counter.
fn get_program_and_sequence_for_pc(
    debug_info: &DebugInfo,
    program_counter: u64,
) -> Result<ProgramLineSequence, DebugError> {
    let program_unit = debug_info.compile_unit_info(program_counter)?;
    let (offset, address_size) = if let Some(line_program) = program_unit.unit.line_program.clone()
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
    if let Some(active_sequence) = line_sequences.iter().find(|line_sequence| {
        line_sequence.start <= program_counter && program_counter < line_sequence.end
    }) {
        Ok(ProgramLineSequence {
            program_unit,
            complete_line_program: complete_line_program.clone(),
            line_sequence: active_sequence.clone(),
        })
    } else {
        Err(DebugError::IncompleteDebugInfo{
                    message: "The specified source location does not have any line information available. Please consider using instruction level stepping.".to_string(),
                    pc_at_error: program_counter,
                })
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
