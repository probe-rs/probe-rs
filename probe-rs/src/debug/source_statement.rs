use super::{unit_info::UnitInfo, DebugError, DebugInfo};
use gimli::{ColumnType, LineSequence};
use num_traits::Zero;
use std::{
    fmt::{Debug, Formatter},
    num::NonZeroU64,
    ops::Range,
};

/// Keep track of all the source statements required to satisfy the operations of [`SteppingMode`].

pub struct SourceStatements {
    // NOTE: Use Vec as a container, because we will have relatively few statements per sequence, and we need to maintain the order.
    pub(crate) statements: Vec<SourceStatement>,
}

impl Debug for SourceStatements {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        for statement in &self.statements {
            writeln!(f, "{:?}", statement)?;
        }
        Ok(())
    }
}

impl SourceStatements {
    /// Extract all the source statements from the `program_unit`, starting at the `program_counter`.
    /// Note:: In the interest of efficiency, for the case of SteppingMode::Breakpoint, this method will return as soon as it finds a valid halt point, and the result will only include the source statements between program_counter and the first valid haltpoint (inclusive).
    pub(crate) fn new(
        debug_info: &DebugInfo,
        program_unit: &UnitInfo,
        program_counter: u64,
    ) -> Result<Self, DebugError> {
        let mut source_statements = SourceStatements {
            statements: Vec::new(),
        };
        let (complete_line_program, active_sequence) =
            get_program_info_at_pc(debug_info, program_unit, program_counter)?;
        let mut sequence_rows = complete_line_program.resume_from(&active_sequence);
        let mut prologue_completed = false;
        let mut source_statement: Option<SourceStatement> = None;
        while let Ok(Some((_, row))) = sequence_rows.next_row() {
            if let Some(source_row) = source_statement.as_mut() {
                if source_row.line.is_none()
                    && row.line().is_some()
                    && row.file_index() == source_row.file_index
                    && row.column() == source_row.column
                {
                    // Workaround the line number issue (it is recorded as None in the DWARF when for debug purposes, it makes more sense to be the same as the previous line).
                    source_row.line = row.line();
                }
            } else {
                // Start tracking the source statement using this row.
                source_statement = Some(SourceStatement::from(row));
            }

            // Don't do anything until we are at least at the prologue_end() of a function.
            if row.prologue_end() {
                prologue_completed = true;
            }

            if !prologue_completed {
                log_row_eval(&active_sequence, program_counter, row, "  inside prologue>");
                continue;
            } else {
                log_row_eval(&active_sequence, program_counter, row, "  after prologue>");
            }

            // Notes about the process of building the source statement:
            // 1. Start a new (and close off the previous) source statement, when we encounter end of sequence OR change of file/line/column.
            // 2. The starting range of the first source statement will always be greater than or equal to the program_counter.
            // 3. The values in the `source_statement` are only updated before we exit the current iteration of the loop, so that we can retroactively close off and store the source statement that belongs to previous `rows`.
            // 4. The debug_info sometimes has a `None` value for the `row.line` that was started in the previous row, in which case we need to carry the previous row `line` number forward. GDB ignores this fact, and it shows up during debug as stepping to the top of the file (line 0) unexpectedly.

            if let Some(source_row) = source_statement.as_mut() {
                // Update the instruction_range.end value.
                source_row.instruction_range = source_row.low_pc()..row.address();

                if row.end_sequence()
                    || (row.is_stmt() && row.address() > source_row.low_pc())
                    || !(row.file_index() == source_row.file_index
                        && (row.line() == source_row.line || row.line().is_none())
                        && row.column() == source_row.column)
                {
                    if source_row.low_pc() >= program_counter {
                        // We need to close off the "current" source statement and add it to the list.
                        source_row.sequence_range = program_counter..active_sequence.end;
                        source_statements.add(source_row.clone());
                    }

                    if row.end_sequence() {
                        // If we hit the end of the sequence, we can get out of here.
                        break;
                    }
                    source_statement = Some(SourceStatement::from(row));
                } else if row.address() == program_counter {
                    // If we encounter the program_counter after the prologue, then we need to use this address as the low_pc, or else we run the risk of setting a breakpoint before the current program counter.
                    source_row.instruction_range = row.address()..row.address();
                }
            }
        }

        if source_statements.len().is_zero() {
            Err(DebugError::NoValidHaltLocation{
                message: "Could not find valid source statements for this address. Consider using instruction level stepping.".to_string(),
                pc_at_error: program_counter,
            })
        } else {
            tracing::trace!(
                "Source statements for pc={:#010x}\n{:?}",
                program_counter,
                source_statements
            );
            Ok(source_statements)
        }
    }

    /// Add a new source statement to the list.
    pub(crate) fn add(&mut self, statement: SourceStatement) {
        self.statements.push(statement);
    }

    /// Get the number of source statements in the list.
    pub(crate) fn len(&self) -> usize {
        self.statements.len()
    }
}

#[derive(Clone)]
/// Keep track of the boundaries of a source statement inside [`gimli::LineSequence`].
/// The `file_index`, `line` and `column` fields from a [`gimli::LineRow`] are used to identify the source statement UNIQUELY in a sequence.
/// Terminology note:
/// - An `instruction` maps to a single machine instruction on target.
/// - A `row` (a [`gimli::LineRow`]) describes the role of an `instruction` in the context of a `sequence`.
/// - A `source_statement` is a range of rows where the addresses of the machine instructions are increasing, but not necessarily contiguous.
/// - A line of code in a source file may contain multiple source statements, in which case a new source statement with unique `column` is created.
/// - The [`gimli::LineRow`] entries for a source statement does not have to be contiguous where they appear in a [`gimli::LineSequence`]
/// - A `sequence`( [`gimli::LineSequence`] ) is a series of contiguous `rows`/`instructions`(may contain multiple `source_statement`'s).
pub(crate) struct SourceStatement {
    /// The first addresss of the statement where row.is_stmt() is true.
    pub(crate) is_stmt: bool,
    pub(crate) file_index: u64,
    pub(crate) line: Option<NonZeroU64>,
    pub(crate) column: ColumnType,
    /// The range of instruction addresses associated with a source statement.
    /// The `address_range.start` is the address of the first instruction which is greater than or equal to the program_counter and not inside the prologue.
    /// The `address_range.end` is the address of the row of the next the sequence, i.e. not part of this statement.
    pub(crate) instruction_range: Range<u64>,
    /// The `sequence_range.start` is the address of the program counter for which this sequence is valid, and allows us to identify target source statements where the program counter lies inside the prologue.
    /// The `sequence_range.end` is the address of the first byte after the end of a sequence, and allows us to identify when stepping over a source statement would result in leaving a sequence.
    pub(crate) sequence_range: Range<u64>,
}

impl Debug for SourceStatement {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "\tStatement={:05} on line={:04}  col={:05}  f={:02}, Range: {:#010x}-{:#010x} --> Sequence Range: {:#010x}-{:#010x}",
            &self.is_stmt,
            match &self.line {
                Some(line) => line.get(),
                None => 0,
            },
            match &self.column {
                gimli::ColumnType::LeftEdge => 0,
                gimli::ColumnType::Column(column) => column.get(),
            },
            &self.file_index,
            &self.instruction_range.start,
            &self.instruction_range.end,
            &self.sequence_range.start,
            &self.sequence_range.end,
        )?;
        Ok(())
    }
}

impl SourceStatement {
    /// Return the first valid halt address of the statement that is greater than or equal to `address`.
    pub(crate) fn get_first_halt_address(&self, address: u64) -> Option<u64> {
        if self.instruction_range.start == address
            || (self.sequence_range.start..self.instruction_range.end).contains(&address)
        {
            Some(self.low_pc())
        } else {
            None
        }
    }

    /// Get the low_pc of this source_statement.
    pub(crate) fn low_pc(&self) -> u64 {
        self.instruction_range.start
    }
}

impl From<&gimli::LineRow> for SourceStatement {
    fn from(line_row: &gimli::LineRow) -> Self {
        SourceStatement {
            is_stmt: line_row.is_stmt(),
            file_index: line_row.file_index(),
            line: line_row.line(),
            column: line_row.column(),
            instruction_range: line_row.address()..line_row.address(),
            sequence_range: line_row.address()..line_row.address(),
        }
    }
}

// Overriding clippy, as this is a private helper function.
#[allow(clippy::type_complexity)]
/// Resolve the relevant program row data for the given program counter.
fn get_program_info_at_pc(
    debug_info: &DebugInfo,
    program_unit: &UnitInfo,
    program_counter: u64,
) -> Result<
    (
        gimli::CompleteLineProgram<
            gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>,
            usize,
        >,
        gimli::LineSequence<gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>>,
    ),
    DebugError,
> {
    let (offset, address_size) = if let Some(line_program) = program_unit.unit.line_program.clone()
    {
        (
            line_program.header().offset(),
            line_program.header().address_size(),
        )
    } else {
        return Err(DebugError::NoValidHaltLocation{
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
        Ok((complete_line_program, active_sequence.clone()))
    } else {
        Err(DebugError::NoValidHaltLocation{
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
    tracing::trace!("Sequence row {:#010X}<={:#010X}<{:#010X}: addr={:#010X} stmt={:5}  ep={:5}  es={:5}  line={:04}  col={:05}  f={:02} : {}",
        active_sequence.start,
        pc,
        active_sequence.end,
        row.address(),
        row.is_stmt(),
        row.prologue_end(),
        row.end_sequence(),
        match row.line() {
            Some(line) => line.get(),
            None => 0,
        },
        match row.column() {
            gimli::ColumnType::LeftEdge => 0,
            gimli::ColumnType::Column(column) => column.get(),
        },
        row.file_index(),
        status);
}
