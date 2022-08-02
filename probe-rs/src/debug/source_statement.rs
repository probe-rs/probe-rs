use crate::debug::source_statement;

use super::{unit_info::UnitInfo, DebugError, DebugInfo, SteppingMode};
use gimli::{ColumnType, LineSequence};
use num_traits::Zero;
use std::{
    cmp::Ordering,
    fmt::{Debug, Formatter},
    iter::zip,
    num::NonZeroU64,
    ops::{Range, RangeBounds, RangeInclusive},
};

#[derive(Clone, Copy)]
/// A private struct to help with the implementation of `SourceStatement`.
struct PriorRow {
    address: u64,
    file_index: u64,
    line: Option<NonZeroU64>,
    column: ColumnType,
}

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
        stepping_mode: &SteppingMode,
    ) -> Result<Self, DebugError> {
        let mut source_statements = SourceStatements {
            statements: Vec::new(),
        };
        let (complete_line_program, active_sequence) =
            get_program_info_at_pc(debug_info, program_unit, program_counter)?;
        let mut sequence_rows = complete_line_program.resume_from(&active_sequence);
        let mut prologue_completed = false;
        let mut source_statement: Option<SourceStatement> = None;
        while let Ok(Some((program_header, row))) = sequence_rows.next_row() {
            // Don't do anything until we are at least at the prologue_end() of a function.
            if row.prologue_end() {
                prologue_completed = true;
            }
            if !prologue_completed {
                log_row_eval(&active_sequence, program_counter, row, "  inside prologue>");
                continue;
            }

            // Notes about the process of building the source statement:
            // 1. Start a new (and close off the previous) source statement, when we encounterend of sequence OR change of file, line or column
            // 2. The starting range of the first source statement will always be greater than or equal to the program_counter. This is because it does not make sense to retun a haltpoint before the PC.
            // 3. The values in the `source_statement` are only updated before we exit the current iteration of the loop, so that we can retroactively close off and store the source statement that belongs to previous `rows`.
            // 4. The debug_info sometimes has a `None` value for the `row.line` that was started in the previous row, in which case we need to carry the previous row `line` number forward.

            // Once past the prologue, we start taking into account the role of the `program_counter`.
            if row.address() < program_counter {
                // Keep track of data belong to the prior row.
                source_statement = Some(SourceStatement::from(row));
                continue;
            } else if source_statement.is_none() {
                source_statement = Some(SourceStatement::from(row));
            }

            // match row.address().cmp(&program_counter) {
            //     Ordering::Greater => {
            //         // Do not update the `source_statement` so that we can retroactively close off the previous row data.
            //     }
            //     Ordering::Less => {
            //         // Keep track of data belong to the prior row.
            //         source_statement = Some(SourceStatement::from(row));
            //         continue;
            //     }
            //     Ordering::Equal => {
            //         if row.line().is_none()
            //         source_statement = Some(SourceStatement::from(row));
            //     }
            // }

            if let Some(source_row) = source_statement.as_mut() {
                if row.line().is_some() && source_row.line.is_none() {
                    source_row.line = row.line();
                }
                source_row.address_range = source_row.low_pc()..row.address();

                // if matches!(stepping_mode, SteppingMode::BreakPoint) && !row.end_sequence() {
                //     // If we are in breakpoint mode, we can get out of here as soon as we find the first valid haltpoint.
                //     source_row.address_range = source_row.low_pc()..row.address();
                //     source_row.sequence_high_pc = active_sequence.end;
                //     source_statements.add(source_row.clone());
                //     log::trace!(
                //         "Source statements for pc={}\n{:?}",
                //         program_counter,
                //         source_statements
                //     );
                //     return Ok(source_statements);
                // } else {
                // If we are starting a new address range, then we need to close previous one.
                if row.end_sequence()
                    || (row.is_stmt() && row.address() > source_row.low_pc())
                    || !(row.file_index() == source_row.file_index
                        && (row.line() == source_row.line || row.line().is_none())
                        && row.column() == source_row.column)
                {
                    // We need to close off the "current" source statement.
                    source_row.sequence_high_pc = active_sequence.end;
                    source_statements.add(source_row.clone());

                    if row.end_sequence() {
                        break;
                    }
                    // Reset the source statement to the current row.
                    source_statement = Some(SourceStatement::from(row));
                }
                // else {
                //     // Update the current source statement with the new row.
                //     source_row.address_range = source_row.low_pc()..row.address();
                // }
                // }
            }
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
            // source_statements
            // .statements
            // .sort_by_key(|statement| (statement.file_index, statement.line, statement.column));
            Ok(source_statements)
        }
    }

    /// Add a new source statement to the list.
    /// If this is a `is_stmt=false`, and a `is_stmt=true` already exists for the same file/line/column, then the latter will be updated with the new address_range.
    /// The `SourceStatement::address_ranges` will be sorted and deduplicated.
    pub(crate) fn add(&mut self, mut statement: SourceStatement) {
        // if !statement.is_stmt {
        //     if let Some(source_statement) =
        //         self.get_mut(statement.file_index, statement.line, statement.column)
        //     {
        //         if statement.high_pc() > source_statement.high_pc() {
        //             source_statement.high_pc() = statement.high_pc();
        //         }
        //         source_statement
        //             .halt_ranges
        //             .append(&mut statement.halt_ranges);
        //         source_statement
        //             .halt_ranges
        //             .sort_by_key(|range| (*range.start(), *range.end()));
        //         source_statement.halt_ranges.dedup();
        //         return;
        //     }
        // }
        self.statements.push(statement);
    }

    /// Get the source statement by file/line/column if the `is_stmnt=true`.
    pub(crate) fn get_mut(
        &mut self,
        file_index: u64,
        line: Option<NonZeroU64>,
        column: ColumnType,
    ) -> Option<&mut SourceStatement> {
        self.statements.iter_mut().find(|s| {
            s.is_stmt == true && s.file_index == file_index && s.line == line && s.column == column
        })
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
    /// All the addresses associated with a source statement.
    /// The `start` is the first address of the sequence, and the `end` is the address of the row of the next the sequence, i.e. not part of this statement.
    pub(crate) address_range: Range<u64>,
    /// The `sequence_high_pc` is the address of the first byte after the end of a sequence.
    pub(crate) sequence_high_pc: u64,
}

impl Debug for SourceStatement {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Statement={:05} on line={:04}  col={:05}  f={:02}, Range: {:#010x}-{:#010x} --> sequence_high_pc={:#010x}",
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
            &self.address_range.start,
            &self.address_range.end,
            &self.sequence_high_pc
        )?;
        Ok(())
    }
}

impl SourceStatement {
    /// Return the first valid halt address of the statement that is greater than or equal to `address`.
    pub(crate) fn get_first_halt_address(&self, address: u64) -> Option<u64> {
        if self.address_range.start == address || self.address_range.contains(&address) {
            Some(self.low_pc())
        } else {
            None
        }
        // if self.address_range.contains(address) {}
        // if (self.low_pc()..self.sequence_high_pc).contains(&address) {
        //     self.halt_ranges
        //         .iter()
        //         .find(|r| r.contains(&address))
        //         // If the range contains the target address, then it is a statement and therefore a valid halt address.
        //         .map(|_| address)
        //         // If not, then find the first statement address after the target address.
        //         .or_else(|| {
        //             zip(self.halt_ranges.iter(), self.halt_ranges.iter().skip(1))
        //                 .find(|(current_range, next_range)| {
        //                     (current_range.end()..=next_range.start()).contains(&&address)
        //                 })
        //                 .map(|(_, next_range)| *next_range.start())
        //         })
        // } else {
        //     None
        // }
    }

    /// Return the statement_high_pc and sequence_high_pc of the statement that contains the `address`.
    pub(crate) fn get_statement_end_points(&self, address: u64) -> Option<(u64, u64)> {
        if (self.low_pc()..self.high_pc()).contains(&address) {
            Some((self.high_pc(), self.sequence_high_pc))
        } else {
            None
        }
    }

    /// Get the low_pc of this source_statement.
    pub(crate) fn low_pc(&self) -> u64 {
        self.address_range.start
    }

    /// Get the high_pc of this source_statement.
    pub(crate) fn high_pc(&self) -> u64 {
        self.address_range.end
    }
}

impl From<&gimli::LineRow> for SourceStatement {
    fn from(line_row: &gimli::LineRow) -> Self {
        SourceStatement {
            is_stmt: line_row.is_stmt(),
            file_index: line_row.file_index(),
            line: line_row.line(),
            column: line_row.column(),
            address_range: line_row.address()..line_row.address(),
            sequence_high_pc: line_row.address(),
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
