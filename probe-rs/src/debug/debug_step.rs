use super::{
    debug_info::DebugInfo,
    source_statement::SourceStatements,
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
    /// Use best efforts to determine the location of any function calls in this statement, and step into them.
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
            .map_err(|error| DebugError::Other(anyhow::anyhow!(error)))?;
        let mut program_counter = match core_status {
            CoreStatus::Halted(_) => core.read_core_reg(core.registers().program_counter())?,
            _ => {
                return Err(DebugError::Other(anyhow::anyhow!(
                    "Core must be halted before stepping."
                )))
            }
        };
        let origin_program_counter = program_counter;
        let mut return_address = core.read_core_reg(core.registers().return_address())?;

        // Sometimes the target program_counter is at a location where the debug_info program row data does not contain valid statements for halt points.
        // When DebugError::NoValidHaltLocation happens, we will step to the next instruction and try again(until we can reasonably expect to have passed out of an epilogue), before giving up.
        let mut target_address: Option<u64> = None;
        for _ in 0..10 {
            match match self {
                SteppingMode::StepInstruction => {
                    // First deal with the the fast/easy case.
                    program_counter = core.step()?.pc;
                    core_status = core.status()?;
                    return Ok((core_status, program_counter));
                }
                SteppingMode::IntoStatement => {
                    self.get_halt_location(Some(core), debug_info, program_counter, None)
                }
                SteppingMode::BreakPoint => {
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
                    // Re-read the program_counter, because it may have changed during the `get_halt_location` call.
                    program_counter = core.read_core_reg(core.registers().program_counter())?;
                    break;
                }
                Err(error) => match error {
                    DebugError::NoValidHaltLocation {
                        message,
                        pc_at_error,
                    } => {
                        // Step on target instruction, and then try again.
                        tracing::trace!(
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
                        tracing::error!("Error during step ({:?}): {}", self, other_error);
                        return Ok((core_status, program_counter));
                    }
                },
            }
        }

        (core_status, program_counter) = match target_address {
            Some(target_address) => {
                tracing::debug!(
                    "Preparing to step ({:20?}): \n\tfrom: {:?} @ {:#010X} \n\t  to: {:?} @ {:#010X}",
                    self,
                    debug_info
                        .get_source_location(program_counter)
                        .map(|source_location| (
                            source_location.file,
                            source_location.line,
                            source_location.column
                        )),
                    origin_program_counter,
                    debug_info
                        .get_source_location(target_address)
                        .map(|source_location| (
                            source_location.file,
                            source_location.line,
                            source_location.column
                        )),
                    target_address,
                );

                run_to_address(program_counter, target_address, core)?
            }
            None => {
                return Err(DebugError::NoValidHaltLocation {
                    message: "Unable to determine target address for this step request."
                        .to_string(),
                    pc_at_error: program_counter,
                });
            }
        };
        Ok((core_status, program_counter))
    }

    /// To understand how this method works, use the following framework:
    /// - Everything is calculated from a given machine instruction address, usually the current program counter.
    /// - To calculate where the user might step to (step-over, step-into, step-out), we start from the given instruction address/program counter, and work our way through all the rows in the sequence of instructions it is part of.
    ///   - A sequence of instructions represents a series of contiguous target machine instructions, and does not necessarily represent the whole of a function.
    ///   - Similarly, the instructions belonging to a source statement are not necessarily contiquous inside the sequence of instructions (e.g. conditional branching inside the sequence).
    ///
    ///
    /// - The next row address in the target processor's instruction sequence may qualify as (one, or more) of the following:
    ///   - The start of a new source statement (a source file may have multiple statements on a single line)
    ///   - Another instruction that is part of the source statement started previously
    ///   - The first instruction after the end of the sequence epilogue.
    ///   - The end of the current sequence of instructions.
    ///   - DWARF defines other flags that are not relevant/used here.
    ///
    ///
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
        let program_unit = get_compile_unit_info(debug_info, program_counter)?;
        match self {
            SteppingMode::BreakPoint => {
                // Find the first_breakpoint_address
                for source_statement in
                    SourceStatements::new(debug_info, &program_unit, program_counter)?.statements
                {
                    if let Some(halt_address) =
                        source_statement.get_first_halt_address(program_counter)
                    {
                        tracing::debug!(
                            "Found first breakpoint {:#010x} for address: {:#010x}",
                            halt_address,
                            program_counter
                        );
                        // We have a good first halt address.
                        let first_breakpoint_address = Some(halt_address);
                        let first_breakpoint_source_location = program_unit
                            .unit
                            .line_program
                            .as_ref()
                            .and_then(|line_program| {
                                line_program
                                    .header()
                                    .file(source_statement.file_index)
                                    .and_then(|file_entry| {
                                        debug_info
                                            .find_file_and_directory(
                                                &program_unit.unit,
                                                line_program.header(),
                                                file_entry,
                                            )
                                            .map(|(file, directory)| SourceLocation {
                                                line: source_statement
                                                    .line
                                                    .map(std::num::NonZeroU64::get),
                                                column: Some(source_statement.column.into()),
                                                file,
                                                directory,
                                                low_pc: Some(source_statement.low_pc() as u32),
                                                high_pc: Some(
                                                    source_statement.instruction_range.end as u32,
                                                ),
                                            })
                                    })
                            });
                        return Ok((first_breakpoint_address, first_breakpoint_source_location));
                    }
                }
            }
            SteppingMode::OverStatement => {
                // Find the next_statement_address
                // - The instructions in a source statement are not necessarily contiguous in the sequence, and the next_statement_address may be affected by conditonal branching at runtime.
                // - Therefore, in order to find the correct next_statement_address, we iterate through the source statements, and :
                //    -- Find the starting address of the next `statement` in the source statements.
                //    -- If there is one, it means the step over target is in the current sequence, so we get the get_first_halt_address() for this next statement.
                //    -- Otherwise the step over target is the same as the step out target.
                let source_statements =
                    SourceStatements::new(debug_info, &program_unit, program_counter)?.statements;
                let mut source_statements_iter = source_statements.iter();
                if let Some((target_address, target_location)) = source_statements_iter
                    .find(|source_statement| {
                        source_statement
                            .instruction_range
                            .contains(&program_counter)
                    })
                    .and_then(|_| {
                        if source_statements.len() == 1 {
                            // Force a SteppingMode::OutOfStatement below.
                            None
                        } else {
                            source_statements_iter.next().and_then(|next_line| {
                                SteppingMode::BreakPoint
                                    .get_halt_location(None, debug_info, next_line.low_pc(), None)
                                    .ok()
                            })
                        }
                    })
                    .or_else(|| {
                        SteppingMode::OutOfStatement
                            .get_halt_location(None, debug_info, program_counter, return_address)
                            .ok()
                    })
                {
                    return Ok((target_address, target_location));
                }
            }
            SteppingMode::IntoStatement => {
                // This is a tricky case because the current RUST generated DWARF, does not store the DW_TAG_call_site information described in the DWARF 5 standard. It is not a mandatory attribute, so not sure if we can ever expect it.
                // To find if any functions are called from the current program counter:
                // 1. Find the statement with the address corresponding to the current PC,
                // 2. Single step the target core, until either ...
                //   (a) We hit a PC that is NOT in the address range of the current statement. This location, which could be any of the following:
                //      (a.i)  A legitimate branch outside the current sequence (call to another instruction) such as a explicit call to a function, or something the compiler injected, like a `drop()`,
                //      (a.ii) An interrupt handler diverted the processing.
                //   (b) We hit a PC that matches the end of the address range, which means there was nothing to step into, so the target is now halted (correctly) at the next statement.
                // TODO: In theory, we could disassemble the instructions in this statement's address range, and find branching instructions, then we would not need to single step the core past the original haltpoint.

                let source_statements =
                    SourceStatements::new(debug_info, &program_unit, program_counter)?.statements;
                let mut source_statements_iter = source_statements.iter();
                if let Some(current_source_statement) =
                    source_statements_iter.find(|source_statement| {
                        source_statement
                            .instruction_range
                            .contains(&program_counter)
                    })
                {
                    if let Some(core) = core {
                        let inclusive_range = current_source_statement.instruction_range.start
                            ..=current_source_statement.instruction_range.end;
                        let (core_status, new_pc) = step_to_address(inclusive_range, core)?;
                        if new_pc == current_source_statement.instruction_range.end {
                            // We have halted at the address after the current statement, so we can conclude there was no branching calls in this sequence.
                            tracing::debug!("Stepping into next statement, but no branching calls found. Stepped to next available statement.");
                        } else if new_pc < current_source_statement.instruction_range.end
                            && matches!(core_status, CoreStatus::Halted(HaltReason::Breakpoint(_)))
                        {
                            // We have halted at a PC that is within the current statement, so there must be another breakpoint.
                            tracing::debug!(
                                "Stepping into next statement, but encountered a breakpoint."
                            );
                        } else {
                            // We have reached a location that is not in the current statement range (branch instruction or breakpoint in an interrupt handler).
                            tracing::debug!(
                                "Stepping into next statement at address: {:#010x}.",
                                new_pc
                            );
                        }

                        return SteppingMode::BreakPoint
                            .get_halt_location(None, debug_info, new_pc, None);
                    }
                }
            }
            SteppingMode::OutOfStatement => {
                if let Ok(function_dies) =
                    program_unit.get_function_dies(program_counter, None, true)
                {
                    // We want the first qualifying (PC is in range) function from the back of this list, to access the 'innermost' functions first.
                    if let Some(function) = function_dies.iter().rev().next() {
                        tracing::trace!(
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
                        } else if function.low_pc <= program_counter
                            && function.high_pc > program_counter
                        {
                            if let Some(core) = core {
                                if function.is_inline() {
                                    // Step_out_address for inlined functions, is the first available breakpoint address after the last statement in the inline function.
                                    let (_, next_instruction_address) =
                                        run_to_address(program_counter, function.high_pc, core)?;
                                    return SteppingMode::BreakPoint.get_halt_location(
                                        None,
                                        debug_info,
                                        next_instruction_address,
                                        None,
                                    );
                                } else if let Some(return_address) = return_address {
                                    tracing::debug!(
                                        "Step Out target: non-inline function, stepping over return address: {:#010x}",
                                            return_address
                                    );
                                    // Step_out_address for non-inlined functions is the first available breakpoint address after the return address.
                                    return SteppingMode::BreakPoint.get_halt_location(
                                        None,
                                        debug_info,
                                        return_address,
                                        None,
                                    );
                                }
                            } else {
                                return Err(DebugError::Other(anyhow::anyhow!("Require a valid `probe_rs::Core::core` to step. Please report this as a bug.")));
                            }
                        }
                    }
                }
            }
            _ => {
                // SteppingMode::StepInstruction is handled in the `step()` method.
            }
        }

        Err(DebugError::NoValidHaltLocation{
                message: "Could not determine valid halt locations for this request. Please consider using instruction level stepping.".to_string(),
                pc_at_error: program_counter,
            })
    }
}

/// Run the target to the desired address. If available, we will use a breakpoint, otherwise we will use single step.
/// Returns the program counter at the end of the step, when any of the following conditions are met:
/// - We reach the `target_address_range.end()` (inclusive)
/// - We reach some other legitimate halt point (e.g. the user tries to step past a series of statements, but there is another breakpoint active in that "gap")
/// - We encounter an error (e.g. the core locks up, or the USB cable is unplugged, etc.)
/// - It turns out this step will be long-running, and we do not have to wait any longer for the request to complete.
fn run_to_address(
    mut program_counter: u64,
    target_address: u64,
    core: &mut Core,
) -> Result<(CoreStatus, u64), DebugError> {
    Ok(if target_address < program_counter {
        // We are not able to calculate a step_out_address. Notify the user to try something else.
        return Err(DebugError::NoValidHaltLocation {
            message: "Unable to determine target address for this step request. Please try a different form of stepping.".to_string(),
            pc_at_error: program_counter,
        });
    } else if target_address == program_counter {
        // No need to step further. e.g. For inline functions we have already stepped to the best available target address..
        (
            core.status()?,
            core.read_core_reg(core.registers().program_counter())?,
        )
    } else if core.set_hw_breakpoint(target_address).is_ok() {
        core.run()?;
        // It is possible that we are stepping over long running instructions.
        match core.wait_for_core_halted(Duration::from_millis(1000)) {
            Ok(()) => {
                // We have hit the target address, so all is good.
                // NOTE: It is conceivable that the core has halted, but we have not yet stepped to the target address. (e.g. the user tries to step out of a function, but there is another breakpoint active before the end of the function.)
                //       This is a legitimate situation, so we clear the breakpoint at the target address, and pass control back to the user
                core.clear_hw_breakpoint(target_address)?;
                (
                    core.status()?,
                    core.read_core_reg(core.registers().program_counter())?,
                )
            }
            Err(error) => {
                program_counter = core.halt(Duration::from_millis(500))?.pc;
                core.clear_hw_breakpoint(target_address)?;
                if matches!(error, crate::Error::Timeout) {
                    // This is not a quick step and halt operation. Notify the user that we are not going to wait any longer, and then return the current program counter so that the debugger can show the user where the forced halt happened.
                    tracing::error!(
                        "The core did not halt after stepping to {:#010X}. Forced a halt at {:#010X}. Long running operations between debug steps are not currently supported.",
                        target_address,
                        program_counter
                    );
                    (core.status()?, program_counter)
                } else {
                    // Something else is wrong.
                    return Err(DebugError::Other(anyhow::anyhow!(
                        "Unexpected error while waiting for the core to halt after stepping to {:#010X}. Forced a halt at {:#010X}. {:?}.",
                        program_counter,
                        target_address,
                        error
                    )));
                }
            }
        }
    } else {
        // If we don't have breakpoints to use, we have to rely on single stepping.
        // TODO: In theory, this could go on for a long time. Should we consider NOT allowing this kind of stepping if there are no breakpoints available?
        step_to_address(target_address..=u64::MAX, core)?
    })
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
                HaltReason::Breakpoint(_) => {
                    tracing::debug!(
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
