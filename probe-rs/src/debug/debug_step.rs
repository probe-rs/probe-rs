// Honestly, I have no idea why this is needed, but without it, there is a clippy warning on a variable called `next_statement_address`
#![allow(unused_assignments)]

use super::{
    debug_info::DebugInfo,
    {DebugError, SourceLocation},
};
use crate::{core::Core, CoreStatus, HaltReason};
use std::{ops::RangeInclusive, time::Duration};

/// Stepping granularity for stepping through a program during debug.
#[derive(Clone, Debug)]
pub enum SteppingMode {
    /// Special case, where we aren't stepping, but we are trying to find the next valid breakpoint.
    /// - The validity of halt locations are defined as target instructions that live between the end of the prologue, and the start of the end sequence of a [`gimli::read::LineRow`].
    BreakPoint,
    /// Advance one machine instruction at a time.
    StepInstruction,
    /// Step Over the current statement, and halt at the start of the next statement.
    OverStatement,
    /// Use best efforts to determin the location of any function calls in this statement, and step into them.
    IntoStatement,
    /// Step to the calling statement, immediately after the current function returns.
    OutOfStatement,
}

impl SteppingMode {
    /// Determine the program counter location where the SteppingMode is aimed, and step to it.
    /// Return the new CoreStatus and program_counter value.
    ///
    /// Implementation Notes for stepping at statement granularity:
    /// - If a hardware breakpoint is available, we will set it at the desired location, run to it, and release it.
    /// - If no hardware breakpoints are available, we will do repeated instruction steps until we reach the desired location.
    ///
    /// Usage Note:
    /// - Currently, no special provision is made for the effect of interrupts that get triggered during stepping. The user must ensure that interrupts are disabled during stepping, or accept that stepping may be diverted by the interrupt processing on the core.
    pub fn step(
        &self,
        core: &mut Core<'_>,
        debug_info: &DebugInfo,
    ) -> Result<(CoreStatus, u64), DebugError> {
        let mut core_status = core
            .status()
            .map_err(|error| DebugError::Other(anyhow::anyhow!(error)))
            .map_err(|error| DebugError::Other(anyhow::anyhow!(error)))?;
        let mut program_counter = match core_status {
            CoreStatus::Halted(_) => core.read_core_reg(core.registers().program_counter())?,
            _ => {
                return Err(DebugError::Other(anyhow::anyhow!(
                    "Core must be halted before stepping."
                )))
            }
        };
        let mut return_address = core.read_core_reg(core.registers().return_address())?;

        // Sometimes the target program_counter is at a location where the debug_info program row data does not contain valid statements for halt points.
        // When DebugError::NoValidHaltLocation happens, we will step to the next instruction and try again(until we can reasonably expect to have passed out of an epilogue), before giving up.
        let mut target_address: Option<u64> = None;
        let mut adjusted_program_counter = program_counter;
        for _ in 0..10 {
            match match self {
                SteppingMode::StepInstruction => {
                    // First deal with the the fast/easy case.
                    program_counter = core.step()?.pc;
                    core_status = core.status()?;
                    return Ok((core_status, program_counter));
                }
                SteppingMode::BreakPoint | SteppingMode::IntoStatement => {
                    self.get_halt_location(None, debug_info, program_counter, None)
                }
                SteppingMode::OverStatement | SteppingMode::OutOfStatement => self
                    .get_halt_location(
                        Some(core),
                        debug_info,
                        program_counter,
                        Some(return_address),
                    ),
            } {
                Ok((post_step_target_address, _)) => {
                    target_address = post_step_target_address;
                    break;
                }
                Err(error) => match error {
                    DebugError::NoValidHaltLocation {
                        message,
                        pc_at_error,
                    } => {
                        // Step on target instruction, and then try again.
                        log::trace!(
                            "Incomplete stepping information @{:#010X}: {}",
                            pc_at_error,
                            message
                        );
                        program_counter = core.step()?.pc;
                        return_address = core.read_core_reg(core.registers().return_address())?;
                        continue;
                    }
                    other_error => {
                        core_status = core.status()?;
                        program_counter = core.read_core_reg(core.registers().program_counter())?;
                        log::error!("Error during step ({:?}): {}", self, other_error);
                        return Ok((core_status, program_counter));
                    }
                },
            }
        }

        match target_address {
            Some(target_address) => {
                log::debug!(
                    "Preparing to step ({:20?}) from PC={:#010X} to: {:#010X}",
                    self,
                    program_counter,
                    target_address
                );

                if target_address == adjusted_program_counter as u64 {
                    // For inline functions we have already stepped to the correct target address..
                } else if core.set_hw_breakpoint(target_address).is_ok() {
                    core.run()?;
                    // It is possible that we are waiting for a breakpoint that is after a long running instruction (e.g. asm::delay_ms(... something greater than 500 ...)).
                    for retries in 0..10 {
                        match core.wait_for_core_halted(Duration::from_millis(500)) {
                            Ok(()) => {
                                // We have hit the target address, so all is good.
                                break;
                            }
                            Err(_) => {
                                if retries == 9 {
                                    // We have waited for a long time, and still haven't hit the target address.
                                    // Force the core to halt.
                                    log::error!("The core did not halt after multiple retries. Forcing a halt.");
                                    core.halt(Duration::from_millis(500))?;
                                } else {
                                    // We have not yet halted, so we need to retry.
                                    log::error!(
                                        "Waiting for the core to halt after stepping to {:#010?}. Retrying ...{}.",
                                        target_address,
                                        retries
                                    );
                                }
                            }
                        };
                    }
                    core_status = match core.status() {
                        Ok(core_status) => {
                            match core_status {
                                CoreStatus::Halted(_) => {
                                    // It is conceivable that the core has halted, but we have not yet stepped to the target address. (e.g. the user tries to step out of a function, but there is another breakpoint active before the end of the function.)
                                    // This is a legitimate situation, so we clear the breakpoint at the target address, and pass control back to the user
                                    core.clear_hw_breakpoint(target_address)?;
                                    adjusted_program_counter =
                                        core.read_core_reg(core.registers().program_counter())?
                                }
                                other => {
                                    log::error!(
                                        "Core should be halted after stepping but is: {:?}",
                                        &other
                                    );
                                    adjusted_program_counter = 0;
                                }
                            };
                            core_status
                        }
                        Err(error) => return Err(DebugError::Probe(error)),
                    };
                } else {
                    // If we don't have breakpoints to use, we have to rely on single stepping.
                    // TODO: In theory, this could go on for a long time. Should we consider NOT allowing this kind of stepping if there are no breakpoints available?
                    (core_status, adjusted_program_counter) =
                        step_to_address(target_address..=u64::MAX, core)?;
                }
            }
            None => {
                return Err(DebugError::NoValidHaltLocation {
                    message: "Unable to determine target address for this step request."
                        .to_string(),
                    pc_at_error: program_counter as u64,
                });
            }
        }
        Ok((core_status, adjusted_program_counter))
    }

    /// To understand how this method works, use the following framework:
    /// - Everything is calculated from a given machine instruction address, usually the current program counter.
    /// - To calculate where the user might step to (step-over, step-into, step-out), we start from the given instruction address/program counter, and work our way through all the rows in the sequence of instructions it is part of. A sequence of instructions represents a series of contiguous target machine instructions, and does not necessarily represent the whole of a function.
    /// - The next row address in the target processor's instruction sequence may qualify as (one, or more) of the following:
    ///   - The start of a new source statement (a source file may have multiple statements on a single line)
    ///   - Another instruction that is part of the source statement started previously
    ///   - The first instruction after the end of the sequence epilogue.
    ///   - The end of the current sequence of instructions.
    ///   - DWARF defines other flags that are not relevant/used here.
    /// - Depending on the combinations of the above, we only use instructions that qualify as:
    ///   - The beginning of a statement that is neither inside the prologue, nor inside the epilogue.
    /// - Based on this, we will attempt to return the "most appropriate" address for the [`SteppingMode`], given the available information in the instruction sequence.
    /// All data is calculated using the [`gimli::read::CompleteLineProgram`] as well as, function call data from the debug info frame section.
    /// NOTE about errors returned: Sometimes the target program_counter is at a location where the debug_info program row data does not contain valid statements for halt points, and we will return a DebugError::NoValidHaltLocation . In this case, we recommend the consumer of this API step the core to the next instruction and try again, with a resasonable retry limit. All other error kinds are should be treated as non recoverable errors.
    pub(crate) fn get_halt_location(
        &self,
        // The core is not required when we are only looking for the next valid breakpoint ( `SteppingMode::Breakpoint` ).
        core: Option<&mut Core<'_>>,
        debug_info: &DebugInfo,
        program_counter: u64,
        return_address: Option<u64>,
    ) -> Result<(Option<u64>, Option<SourceLocation>), DebugError> {
        let (program_unit, complete_line_program, active_sequence) =
            get_program_info_at_pc(debug_info, program_counter)?;

        // For `OutOfStatement`, we do not need to loop through program rows.
        if matches!(self, SteppingMode::OutOfStatement) {
            if let Ok(function_dies) = program_unit.get_function_dies(program_counter, None, true) {
                // We want the first qualifying (PC is in range) function from the back of this list, to access the 'innermost' functions first.
                if let Some(function) = function_dies.iter().rev().next() {
                    log::trace!(
                        "Step Out target: Evaluating function {:?}, low_pc={:?}, high_pc={:?}",
                        function.function_name(),
                        function.low_pc,
                        function.high_pc
                    );
                    if function.get_attribute(gimli::DW_AT_noreturn).is_some() {
                        return Err(DebugError::Other(anyhow::anyhow!(
                        "Function {:?} is marked as `noreturn`. Cannot step out of this function.",
                        function.function_name()
                    )));
                    } else if function.low_pc <= program_counter as u64
                        && function.high_pc > program_counter as u64
                    {
                        if function.is_inline() {
                            if let Some(core) = core {
                                // Step_out_address for inlined functions, is the first available breakpoint address after the last statement in the inline function.
                                let (_, next_instruction_address) =
                                    step_to_address(program_counter..=function.high_pc, core)?;
                                return SteppingMode::BreakPoint.get_halt_location(
                                    None,
                                    debug_info,
                                    next_instruction_address,
                                    None,
                                );
                            } else {
                                return Err(DebugError::Other(anyhow::anyhow!("Require a valid `probe_rs::Core::core` to step. Please report this as a bug.")));
                            }
                        } else if let Some(return_address) = return_address {
                            // Step_out_address for non-inlined functions is the first available breakpoint address after the return address.
                            return SteppingMode::BreakPoint.get_halt_location(
                                None,
                                debug_info,
                                return_address,
                                None,
                            );
                        }
                    }
                }
            }
        }

        // Setup a couple of variables to track the current state of the discovery process.
        let mut first_breakpoint_address = None;
        let mut first_breakpoint_source_location = None;
        let mut next_statement_address = None;
        let mut prologue_completed = false;
        let mut prior_row_address = None;

        let mut sequence_rows = complete_line_program.resume_from(&active_sequence);
        while let Ok(Some((program_header, row))) = sequence_rows.next_row() {
            if row.end_sequence() {
                // If we encounter a end_sequence(), we will need to know what the prior row was, so do not update it.
            } else {
                prior_row_address = Some(row.address());
            }

            // Don't do anything until we are at least at the prologue_end() of a function.
            if row.prologue_end() {
                prologue_completed = true;
            }
            if !prologue_completed {
                log_row_eval(program_counter, row, "  <prologue>");
                continue;
            }

            // NOTE: row.end_sequence() is a row whose address is that of the byte after the last target machine instruction of the sequence.
            // - At this point, the program_counter register is no longer inside the code of the sequence.
            // - IMPORTANT: Because of the above, we will NOT allow a breakpoint, or a step target to be on a statement that is a row.end_sequence()

            // PART 1: Find the first_breakpoint_address
            if first_breakpoint_address.is_none() && row.address() >= program_counter {
                if row.end_sequence() {
                    log_row_eval(program_counter, row, "  <end sequence>");
                    // If the first non-prologue row is a end of sequence, then we cannot determine valid halt addresses at this program counter.
                    return Err(DebugError::NoValidHaltLocation{
                            message: "This function does not have any valid halt locations. Please consider using instruction level stepping.".to_string(),
                            pc_at_error: program_counter,
                        });
                } else if row.is_stmt() {
                    // We have a good first halt address.
                    log_row_eval(program_counter, row, "<first_halt_address>");
                    first_breakpoint_address = Some(row.address());
                    if let Some(file_entry) = row.file(program_header) {
                        if let Some((file, directory)) = debug_info.find_file_and_directory(
                            &program_unit.unit,
                            program_header,
                            file_entry,
                        ) {
                            first_breakpoint_source_location = Some(SourceLocation {
                                line: row.line().map(std::num::NonZeroU64::get),
                                column: Some(row.column().into()),
                                file,
                                directory,
                                low_pc: Some(active_sequence.start as u32),
                                high_pc: Some(active_sequence.end as u32),
                            });
                        }
                    }
                    if matches!(self, SteppingMode::BreakPoint) {
                        return Ok((first_breakpoint_address, first_breakpoint_source_location));
                    } else {
                        continue;
                    }
                } else {
                    log_row_eval(program_counter, row, "  <non-statement>");
                    continue;
                }
            }

            // PART 2: Set the next_statement_address
            if first_breakpoint_address.is_some()
                && next_statement_address.is_none()
                && row.address() > program_counter
            {
                if row.end_sequence() {
                    log_row_eval(program_counter, row, "  <end sequence>");
                    log::warn!("The sequence at PC={:#010x} does not have a valid next statement address. The core will be stepped until it encounters the next valid statement in a subsequent sequence.", program_counter);
                    // If the current sequence does not have a valid next statement address, then:
                    // - Because we have no way of knowing where the next sequence of instructions:
                    //   - The core will have to be stepped to the end of this sequence (the row prior to end_sequence),
                    //   - And then step one more time,
                    //   - And then determine the `first_halt_address` at that location.
                    if let Some(prior_row_address) = prior_row_address {
                        if let Some(core) = core {
                            step_to_address(program_counter..=prior_row_address, core)?;
                            let next_instruction_address = core.step()?.pc;
                            next_statement_address = SteppingMode::BreakPoint
                                .get_halt_location(
                                    None,
                                    debug_info,
                                    next_instruction_address,
                                    None,
                                )?
                                .0;
                        } else {
                            return Err(DebugError::Other(anyhow::anyhow!("Require a valid `probe_rs::Core::core` to step. Please report this as a bug.")));
                        }
                    }
                    // If the value is still None, this function will exit with an Err().
                    break;
                } else if row.is_stmt() {
                    log_row_eval(program_counter, row, "<next_statement_address>");
                    // Use the next available statement.
                    next_statement_address = Some(row.address());
                } else {
                    log_row_eval(program_counter, row, "  <non-statement>");
                    continue;
                }
                if matches!(self, SteppingMode::OverStatement) {
                    return Ok((next_statement_address, None));
                } else {
                    // We can move to the next row.
                    continue;
                }
            }

            // PART 3: Find the step_into_address
            if matches!(self, SteppingMode::IntoStatement) {
                // This is a tricky case because the current RUST generated DWARF, does not store the DW_TAG_call_site information described in the DWARF 5 standard. It is not a mandatory attribute, so not sure if we can ever expect it.
                // To find if any functions are called from the current program counter:
                // - Start at the current PC,
                // - Single step the target core, until either ...
                //   (a) We hit a PC that is not in the current sequence between starting PC and the address last row in this sequence. Halt at this location, which could be any of the following:
                //      (a.i)  A legitimate branch (call to another instruction) such as a explicit call to a function, or something the compiler injected, like a `drop()`,
                //      (a.ii) An interrupt handler diverted the processing.
                //   (b) We hit a PC that matches the next valid statement stored above, which means there was nothing to step into, so the target is now halted (correctly) at the `next_halt_address`
                let target_address = if let (
                    Some(first_breakpoint_address),
                    Some(next_statement_address),
                ) = (first_breakpoint_address, next_statement_address)
                {
                    if let Some(core) = core {
                        let next_pc = step_to_address(
                            first_breakpoint_address..=next_statement_address,
                            core,
                        )?
                        .1;
                        if next_pc == next_statement_address {
                            // We have reached the next_statement_address, so we can conclude there was no branching calls in this sequence.
                            log::warn!("Stepping into next statement, but no branching calls found. Stepped to next available statement.");
                            next_pc
                        } else {
                            // We have reached a location that is not in the current sequence, so we can conclude there was a branching call in this sequence.
                            // We will halt at the first valid breakpoint address after this point.
                            if let (Some(next_valid_halt_address), _) = SteppingMode::BreakPoint
                                .get_halt_location(None, debug_info, next_pc, None)?
                            {
                                log::debug!("Stepping into next statement, and a branching call was found. Stepping to next valid halt address: {:#010x}.", next_valid_halt_address);
                                next_valid_halt_address
                            } else {
                                log::debug!("Stepping into next statement, and a branching call was found, but no next valid halt address. Halted at  {:#010x}.", next_pc);
                                next_pc
                            }
                        }
                    } else {
                        return Err(DebugError::Other(anyhow::anyhow!("Require a valid `probe_rs::Core::core` to step. Please report this as a bug.")));
                    }
                } else {
                    // Our technique requires a valid first_breakpoint_address AND a valid next_statement_address be computed before we can do this.
                    continue;
                };
                return Ok((Some(target_address), None));
            }
        }

        // PART 3: In the unlikely scenario that we encounter a sequence of statements that complete before we encounter `row.prologue_end()` or `row.end_sequence`, then we will arrive at this point with no halt location information.
        Err(DebugError::NoValidHaltLocation{
                message: "Could not determine valid halt locations for this request. Please consider using instruction level stepping.".to_string(),
                pc_at_error: program_counter,
            })
    }
}

/// In some cases, we need to single-step the core, until ONE of the following conditions are met:
/// - We reach the `target_address_range.end()` (inclusive)
/// - We reach an address that is not in the sequential range of `target_address_range` (inclusive), i.e. we stepped to some kind of branch instruction.
/// - We reach some other legitimate halt point (e.g. the user tries to step past a series of statements, but there is another breakpoint active in that "gap")
/// - We encounter an error (e.g. the core locks up)
fn step_to_address(
    target_address_range: RangeInclusive<u64>,
    core: &mut Core,
) -> Result<(CoreStatus, u64), DebugError> {
    while target_address_range.contains(&core.step()?.pc) {
        // Single step the core until we get to the target_address;

        match core.status()? {
            CoreStatus::Halted(halt_reason) => match halt_reason {
                HaltReason::Step | HaltReason::Request => continue,
                HaltReason::Breakpoint => {
                    log::debug!(
                        "Encountered a breakpoint before the target address ({:#010x}) was reached.",
                        target_address_range.end()
                    );
                    break;
                }
                // This is a recoverable error kind, and can be reported to the user higher up in the call stack.
                other_halt_reason => return Err(DebugError::NoValidHaltLocation{message: format!("Target halted unexpectedly before we reached the destination address of a step operation: {:?}", other_halt_reason), pc_at_error: core.read_core_reg(core.registers().program_counter())?}),
            },
            // This is not a recoverable error, and will result in the debug session ending (we have no predicatable way of successfully continuing the session)
            other_status => return Err(DebugError::Other(anyhow::anyhow!("Target failed to reach the destination address of a step operation: {:?}", other_status))),
        }
    }
    Ok((
        core.status()?,
        core.read_core_reg(core.registers().program_counter())?,
    ))
}

/// Helper function to avoid code duplication when logging of information during row evaluation.
fn log_row_eval(pc: u64, row: &gimli::LineRow, status: &str) {
    log::trace!("Sequence row data @PC={:#010X} addr={:#010X} stmt={:5}  ep={:5}  es={:5}  line={:04}  col={:05}  f={:02} : {}",
        pc,
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

// Overriding clippy, as this is a private helper function.
#[allow(clippy::type_complexity)]
/// Resolve the relevant program row data for the given program counter.
fn get_program_info_at_pc(
    debug_info: &DebugInfo,
    program_counter: u64,
) -> Result<
    (
        super::unit_info::UnitInfo,
        gimli::CompleteLineProgram<
            gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>,
            usize,
        >,
        gimli::LineSequence<gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>>,
    ),
    DebugError,
> {
    let program_unit = get_compile_unit_info(debug_info, program_counter)?;
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
        Ok((program_unit, complete_line_program, active_sequence.clone()))
    } else {
        Err(DebugError::NoValidHaltLocation{
                    message: "The specified source location does not have any line information available. Please consider using instruction level stepping.".to_string(),
                    pc_at_error: program_counter,
                })
    }
}

/// Find the compile unit at the current address.
fn get_compile_unit_info(
    debug_info: &DebugInfo,
    program_counter: u64,
) -> Result<super::unit_info::UnitInfo, DebugError> {
    let mut units = debug_info.get_units();
    while let Some(header) = debug_info.get_next_unit_info(&mut units) {
        match debug_info.dwarf.unit_ranges(&header.unit) {
            Ok(mut ranges) => {
                while let Ok(Some(range)) = ranges.next() {
                    if (range.begin <= program_counter) && (range.end > program_counter) {
                        return Ok(header);
                    }
                }
            }
            Err(_) => continue,
        };
    }
    Err(DebugError::NoValidHaltLocation{
        message: "The specified source location does not have any debug information available. Please consider using instruction level stepping.".to_string(),
        pc_at_error: program_counter,
    })
}
