use super::{unit_info::UnitInfo, DebugError, DebugInfo, SteppingMode};
use gimli::{ColumnType, LineSequence};
use num_traits::Zero;
use std::{
    fmt::{Debug, Formatter},
    iter::zip,
    num::NonZeroU64,
    ops::RangeInclusive,
};

/// Keep track of all the source statements required to satisfy the operations of [`SteppingMode`].
/// These source statements may extend beyond the boundaries of a single [`gimli::LineSequence`]
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
        stepping_mode: &SteppingMode,
    ) -> Result<Self, DebugError> {
        let mut source_statements = SourceStatements {
            statements: Vec::new(),
        };
        let (complete_line_program, active_sequence) =
            get_program_info_at_pc(debug_info, program_unit, program_counter)?;
        let mut sequence_rows = complete_line_program.resume_from(&active_sequence);
        let mut prologue_completed = false;
        #[derive(Clone, Copy)]
        struct PriorRow {
            address: u64,
            file_index: u64,
            line: Option<NonZeroU64>,
            column: ColumnType,
        }
        let mut prior_row_in_sequence: Option<PriorRow> = None;
        let mut current_address_range_start = None;
        while let Ok(Some((program_header, row))) = sequence_rows.next_row() {
            // Don't do anything until we are at least at the prologue_end() of a function.
            if row.prologue_end() {
                prologue_completed = true;
            }
            if !prologue_completed {
                log_row_eval(&active_sequence, program_counter, row, "  inside prologue>");
                continue;
            }

            // Once past the prologue, skip over any rows that refer to addresses before the `program_counter`.
            // Also skip past any rows that are not statements (except for end of sequence ... we need those).
            if row.address() < program_counter && !(row.is_stmt() || row.end_sequence()) {
                continue;
            }

            if matches!(stepping_mode, SteppingMode::BreakPoint)
                && !row.end_sequence()
                && row.address() >= program_counter
            {
                // If we are in breakpoint mode, we can get out of here as soon as we find the first valid haltpoint.
                source_statements.add(SourceStatement {
                    high_pc: active_sequence.end,
                    halt_ranges: vec![row.address()..=row.address()],
                    file_index: row.file_index(),
                    line: row.line(),
                    column: row.column(),
                });
                log::trace!(
                    "Source statements for pc={}\n{:?}",
                    program_counter,
                    source_statements
                );
                return Ok(source_statements);
            } else if let (Some(address_range_start), Some(prior_row)) =
                (current_address_range_start, prior_row_in_sequence)
            {
                // If we need to close off the current address range.
                if row.end_sequence()
                    || !(row.file_index() == prior_row.file_index
                        && (row.line() == prior_row.line || row.line().is_none())
                        && row.column() == prior_row.column)
                {
                    // We need to close off the "current" source statement.
                    source_statements.add(SourceStatement {
                        high_pc: active_sequence.end,
                        halt_ranges: vec![address_range_start..=prior_row.address],
                        file_index: prior_row.file_index,
                        line: prior_row.line,
                        column: prior_row.column,
                    });
                    if row.end_sequence() {
                        break;
                    } else {
                        current_address_range_start = Some(row.address());
                    }
                }
            } else if !row.end_sequence() {
                current_address_range_start = Some(row.address());
            }

            // Store this, so we use it to determine end of statement ranges.
            // There are cases where the line is None, when it should be the same as the previous row.
            // If we don't "fix" this, we end up with situations where debuggers like gdb will "confusingly" jump to the top of the file while stepping.
            let mut partial_of_current_row = PriorRow {
                address: row.address(),
                file_index: row.file_index(),
                line: row.line(),
                column: row.column(),
            };
            if partial_of_current_row.line.is_none() {
                if let Some(prior_row_in_sequence) = prior_row_in_sequence {
                    if prior_row_in_sequence.file_index == partial_of_current_row.file_index {
                        partial_of_current_row.line = prior_row_in_sequence.line;
                    }
                }
            }
            prior_row_in_sequence = Some(partial_of_current_row);
        }

        if source_statements.len().is_zero() {
            Err(DebugError::NoValidHaltLocation{
                message: "Could not find valid source statements for this address. Consider using instruction level stepping.".to_string(),
                pc_at_error: program_counter,
            })
        } else {
            log::trace!(
                "Source statements for pc={}\n{:?}",
                program_counter,
                source_statements
            );
            Ok(source_statements)
        }
    }

    /// Add a new source statement to the list.
    /// If it already exists, it will be updated with the new address_range.
    /// The `SourceStatement::address_ranges` will be sorted and deduplicated.
    pub(crate) fn add(&mut self, mut statement: SourceStatement) {
        if let Some(source_statement) =
            self.get_mut(statement.file_index, statement.line, statement.column)
        {
            if statement.high_pc > source_statement.high_pc {
                source_statement.high_pc = statement.high_pc;
            }
            source_statement
                .halt_ranges
                .append(&mut statement.halt_ranges);
            source_statement
                .halt_ranges
                .sort_by_key(|range| (*range.start(), *range.end()));
            source_statement.halt_ranges.dedup();
        } else {
            self.statements.push(statement);
        }
    }

    /// Get the source statement by file/line/column.
    pub(crate) fn get_mut(
        &mut self,
        file_index: u64,
        line: Option<NonZeroU64>,
        column: ColumnType,
    ) -> Option<&mut SourceStatement> {
        self.statements
            .iter_mut()
            .find(|s| s.file_index == file_index && s.line == line && s.column == column)
    }

    /// Get the number of source statements in the list.
    pub(crate) fn len(&self) -> usize {
        self.statements.len()
    }
}

/// Keep track of the boundaries of a source statement inside [`gimli::LineSequence`].
/// The `file_index`, `line` and `column` fields from a [`gimli::LineRow`] are used to identify the source statement UNIQUELY in a sequence.
pub(crate) struct SourceStatement {
    /// The first addresss of the statement where row.is_stmt() is true.
    pub(crate) file_index: u64,
    pub(crate) line: Option<NonZeroU64>,
    pub(crate) column: ColumnType,
    /// The `high_pc` is the first address after the last address of the statement.
    high_pc: u64,
    /// All the addresses of valid halt_addresses in the statements of the active sequence.
    /// The `start` is the first address of the sequence, and the `end` is the last valid halt address of the sequence.
    /// These ranges are sorted and deduplicated (i.e. no overlapping ranges).
    pub(crate) halt_ranges: Vec<RangeInclusive<u64>>,
}

impl Debug for SourceStatement {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Statement line={:04}  col={:05}  f={:02}, Ranges:",
            match &self.line {
                Some(line) => line.get(),
                None => 0,
            },
            match &self.column {
                gimli::ColumnType::LeftEdge => 0,
                gimli::ColumnType::Column(column) => column.get(),
            },
            &self.file_index,
        )?;
        for address_range in &self.halt_ranges {
            write!(
                f,
                " {:#010x}-{:#010x},",
                address_range.start(),
                address_range.end()
            )?;
        }
        Ok(())
    }
}

impl SourceStatement {
    pub(crate) fn new(
        file_index: u64,
        line: Option<NonZeroU64>,
        column: ColumnType,
        high_pc: u64,
    ) -> Self {
        Self {
            file_index,
            line,
            column,
            high_pc,
            halt_ranges: Vec::new(),
        }
    }

    /// Return the first valid halt address of the statement that is greater than or equal to `address`.
    pub(crate) fn get_halt_address(&self, address: u64) -> Option<u64> {
        if (self.low_pc()..self.high_pc()).contains(&address) {
            self.halt_ranges
                .iter()
                .find(|r| r.contains(&address))
                // If the range contains the target address, then it is a statement and therefore a valid halt address.
                .map(|_| address)
                // If not, then find the first statement address after the target address.
                .or_else(|| {
                    zip(self.halt_ranges.iter(), self.halt_ranges.iter().skip(1))
                        .find(|(current_range, next_range)| {
                            (current_range.end()..=next_range.start()).contains(&&address)
                        })
                        .map(|(_, next_range)| *next_range.start())
                })
        } else {
            None
        }
    }
    /// Return (if any) a valid halt_range that qualifies as:
    /// - Lies between the low_pc and high_pc of the statement addresses
    /// - Is the range that either contains, or immediately follows the given address (ei.. there are no earlier ranges that cover this address)
    /// NOTE: The result range my have a start() that is less than the given address.
    pub(crate) fn get_halt_range(&self, address: u64) -> Option<&RangeInclusive<u64>> {
        self.get_halt_address(address).and_then(|halt_address| {
            self.halt_ranges
                .iter()
                .rev()
                .find(|r| r.contains(&halt_address))
        })
    }

    /// Get the high_pc of this source_statement.
    /// This value is maintained in SourceStatements::add()
    pub(crate) fn high_pc(&self) -> u64 {
        self.high_pc
    }

    /// Get the low_pc of this source_statement.
    /// This value is computed by finding the lowest address in the address_ranges.
    pub(crate) fn low_pc(&self) -> u64 {
        *self
            .halt_ranges
            .iter()
            .map(|r| r.start())
            .min()
            .unwrap_or(&0)
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
    log::trace!("Sequence row {:#010X}<={:#010X}<{:#010X}: addr={:#010X} stmt={:5}  ep={:5}  es={:5}  line={:04}  col={:05}  f={:02} : {}",
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
