use super::{debug_info::DebugInfo, DebugError, VerifiedBreakpoint};
use crate::{
    architecture::{
        arm::ArmError, riscv::communication_interface::RiscvError,
        xtensa::communication_interface::XtensaError,
    },
    CoreInterface, CoreStatus, HaltReason,
};
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
    /// - Currently, no special provision is made for the effect of interrupts that get triggered
    ///   during stepping. The user must ensure that interrupts are disabled during stepping, or
    ///   accept that stepping may be diverted by the interrupt processing on the core.
    pub fn step(
        &self,
        core: &mut impl CoreInterface,
        debug_info: &DebugInfo,
    ) -> Result<(CoreStatus, u64), DebugError> {
        let mut core_status = core.status()?;
        let mut program_counter = match core_status {
            CoreStatus::Halted(_) => core
                .read_core_reg(core.program_counter().id())?
                .try_into()?,
            _ => {
                return Err(DebugError::Other(
                    "Core must be halted before stepping.".to_string(),
                ))
            }
        };
        let origin_program_counter = program_counter;
        let mut return_address = core.read_core_reg(core.return_address().id())?.try_into()?;

        // Sometimes the target program_counter is at a location where the debug_info program row data does not contain valid statements for halt points.
        // When DebugError::NoValidHaltLocation happens, we will step to the next instruction and try again(until we can reasonably expect to have passed out of an epilogue), before giving up.
        let mut target_address: Option<u64> = None;
        for _ in 0..10 {
            let post_step_target = match self {
                SteppingMode::StepInstruction => {
                    // First deal with the the fast/easy case.
                    program_counter = core.step()?.pc;
                    core_status = core.status()?;
                    return Ok((core_status, program_counter));
                }
                SteppingMode::BreakPoint => {
                    self.get_halt_location(core, debug_info, program_counter, None)
                }
                SteppingMode::IntoStatement
                | SteppingMode::OverStatement
                | SteppingMode::OutOfStatement => {
                    // The more complex cases, where specific handling is required.
                    self.get_halt_location(core, debug_info, program_counter, Some(return_address))
                }
            };
            match post_step_target {
                Ok(post_step_target) => {
                    target_address = Some(post_step_target.address);
                    // Re-read the program_counter, because it may have changed during the `get_halt_location` call.
                    program_counter = core
                        .read_core_reg(core.program_counter().id())?
                        .try_into()?;
                    break;
                }
                Err(error) => {
                    match error {
                        DebugError::WarnAndContinue { message } => {
                            // Step on target instruction, and then try again.
                            tracing::trace!("Incomplete stepping information @{program_counter:#010X}: {message}");
                            program_counter = core.step()?.pc;
                            return_address =
                                core.read_core_reg(core.return_address().id())?.try_into()?;
                            continue;
                        }
                        other_error => {
                            core_status = core.status()?;
                            program_counter = core
                                .read_core_reg(core.program_counter().id())?
                                .try_into()?;
                            tracing::error!("Error during step ({:?}): {}", self, other_error);
                            return Ok((core_status, program_counter));
                        }
                    }
                }
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
                            source_location.path,
                            source_location.line,
                            source_location.column
                        )),
                    origin_program_counter,
                    debug_info
                        .get_source_location(target_address)
                        .map(|source_location| (
                            source_location.path,
                            source_location.line,
                            source_location.column
                        )),
                    target_address,
                );

                run_to_address(program_counter, target_address, core)?
            }
            None => {
                return Err(DebugError::WarnAndContinue {
                    message: "Unable to determine target address for this step request."
                        .to_string(),
                });
            }
        };
        Ok((core_status, program_counter))
    }

    /// To understand how this method works, use the following framework:
    /// - Everything is calculated from a given machine instruction address, usually the current program counter.
    /// - To calculate where the user might step to (step-over, step-into, step-out), we start from the given instruction
    ///     address/program counter, and work our way through all the rows in the sequence of instructions it is part of.
    ///   - A sequence of instructions represents a series of monotonically increasing target machine instructions,
    ///     and does not necessarily represent the whole of a function.
    ///   - Similarly, the instructions belonging to a sequence are not necessarily contiguous inside the sequence of instructions,
    ///     e.g. conditional branching inside the sequence.
    /// - To determine valid halt points for breakpoints and stepping, we only use instructions that qualify as:
    ///   - The beginning of a statement that is neither inside the prologue, nor inside the epilogue.
    /// - Based on this, we will attempt to return the "most appropriate" address for the [`SteppingMode`], given the available information in the instruction sequence.
    ///
    /// All data is calculated using the [`gimli::read::CompleteLineProgram`] as well as, function call data from the debug info frame section.
    ///
    /// NOTE about errors returned: Sometimes the target program_counter is at a location where the debug_info program row data does not contain valid statements
    /// for halt points, and we will return a `DebugError::NoValidHaltLocation`. In this case, we recommend the consumer of this API step the core to the next instruction
    /// and try again, with a reasonable retry limit. All other error kinds are should be treated as non recoverable errors.
    pub(crate) fn get_halt_location(
        &self,
        core: &mut impl CoreInterface,
        debug_info: &DebugInfo,
        program_counter: u64,
        return_address: Option<u64>,
    ) -> Result<VerifiedBreakpoint, DebugError> {
        let program_unit = debug_info.compile_unit_info(program_counter)?;
        match self {
            SteppingMode::BreakPoint => {
                // Find the first_breakpoint_address
                return VerifiedBreakpoint::for_address(debug_info, program_counter);
            }
            SteppingMode::OverStatement => {
                // Find the "step over location"
                // - The instructions in a sequence do not necessarily have contiguous addresses,
                //   and the next instruction address may be affected by conditonal branching at runtime.
                // - Therefore, in order to find the correct "step over location", we iterate through the
                //   instructions to find the starting address of the next halt location, ie. the address
                //   is greater than the current program counter.
                //    -- If there is one, it means the step over target is in the current sequence,
                //       so we get the valid breakpoint location for this next location.
                //    -- If there is not one, the step over target is the same as the step out target.
                return VerifiedBreakpoint::for_address(
                    debug_info,
                    program_counter.saturating_add(1),
                )
                .or_else(|_| {
                    // If we cannot find a valid breakpoint in the current sequence, we will step out of the current sequence.
                    SteppingMode::OutOfStatement.get_halt_location(
                        core,
                        debug_info,
                        program_counter,
                        return_address,
                    )
                });
            }
            SteppingMode::IntoStatement => {
                // This is a tricky case because the current RUST generated DWARF, does not store the DW_TAG_call_site information described in the DWARF 5 standard.
                // - It is not a mandatory attribute, so not sure if we can ever expect it.
                // To find if any functions are called from the current program counter:
                // 1. Identify the next instruction location after the instruction corresponding to the current PC,
                // 2. Single step the target core, until either of the following:
                //   (a) We hit a PC that is NOT in the range between the current PC and the next instruction location.
                //       This location, which could be any of the following:
                //          (a.i)  A legitimate branch outside the current sequence (call to another instruction) such as
                //                 an explicit call to a function, or something the compiler injected, like a `drop()`,
                //          (a.ii) An interrupt handler diverted the processing.
                //   (b) We hit a PC at the address of the identified next instruction location,
                //       which means there was nothing to step into, so the target is now halted (correctly) at the next statement.
                let target_pc = match VerifiedBreakpoint::for_address(
                    debug_info,
                    program_counter.saturating_add(1),
                ) {
                    Ok(identified_next_breakpoint) => identified_next_breakpoint.address,
                    Err(DebugError::WarnAndContinue { .. }) => {
                        // There are no next statements in this sequence, so we will use the return address as the target.
                        if let Some(return_address) = return_address {
                            return_address
                        } else {
                            return Err(DebugError::WarnAndContinue {
                                message: "Could not determine a 'step in' target. Please use 'step over'.".to_string(),
                            });
                        }
                    }
                    Err(other_error) => {
                        return Err(other_error);
                    }
                };

                let (core_status, new_pc) = step_to_address(program_counter..=target_pc, core)?;
                if (program_counter..=target_pc).contains(&new_pc) {
                    // We have halted at an address after the current instruction (either in the same sequence,
                    // or at the return address of the current function),
                    // so we can conclude there were no branching calls in this instruction.
                    tracing::debug!("Stepping into next statement, but no branching calls found. Stepped to next available location.");
                } else if matches!(core_status, CoreStatus::Halted(HaltReason::Breakpoint(_))) {
                    // We have halted at a PC that is within the current statement, so there must be another breakpoint.
                    tracing::debug!("Stepping into next statement, but encountered a breakpoint.");
                } else {
                    tracing::debug!("Stepping into next statement at address: {:#010x}.", new_pc);
                }

                return SteppingMode::BreakPoint.get_halt_location(core, debug_info, new_pc, None);
            }
            SteppingMode::OutOfStatement => {
                if let Ok(function_dies) =
                    program_unit.get_function_dies(debug_info, program_counter)
                {
                    // We want the first qualifying (PC is in range) function from the back of this list,
                    // to access the 'innermost' functions first.
                    if let Some(function) = function_dies.iter().next_back() {
                        tracing::trace!(
                            "Step Out target: Evaluating function {:?}, low_pc={:?}, high_pc={:?}",
                            function.function_name(debug_info),
                            function.low_pc(),
                            function.high_pc()
                        );

                        if function
                            .attribute(debug_info, gimli::DW_AT_noreturn)
                            .is_some()
                        {
                            return Err(DebugError::Other(format!(
                                "Function {:?} is marked as `noreturn`. Cannot step out of this function.",
                                function.function_name(debug_info).as_deref().unwrap_or("<unknown>")
                            )));
                        } else if function.range_contains(program_counter) {
                            if function.is_inline() {
                                // Step_out_address for inlined functions, is the first available breakpoint address after the last statement in the inline function.
                                let (_, next_instruction_address) = run_to_address(
                                    program_counter,
                                    function.high_pc().unwrap(), //unwrap is OK because `range_contains` is true.
                                    core,
                                )?;
                                return SteppingMode::BreakPoint.get_halt_location(
                                    core,
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
                                    core,
                                    debug_info,
                                    return_address,
                                    None,
                                );
                            }
                        }
                    }
                }
            }
            _ => {
                // SteppingMode::StepInstruction is handled in the `step()` method.
            }
        }

        Err(DebugError::WarnAndContinue {
                message: "Could not determine valid halt locations for this request. Please consider using instruction level stepping.".to_string()
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
    core: &mut impl CoreInterface,
) -> Result<(CoreStatus, u64), DebugError> {
    Ok(if target_address == program_counter {
        // No need to step further. e.g. For inline functions we have already stepped to the best available target address..
        (
            core.status()?,
            core.read_core_reg(core.program_counter().id())?
                .try_into()?,
        )
    } else if core.set_hw_breakpoint(0, target_address).is_ok() {
        core.run()?;
        // It is possible that we are stepping over long running instructions.
        match core.wait_for_core_halted(Duration::from_millis(1000)) {
            Ok(()) => {
                // We have hit the target address, so all is good.
                // NOTE: It is conceivable that the core has halted, but we have not yet stepped to the target address. (e.g. the user tries to step out of a function, but there is another breakpoint active before the end of the function.)
                //       This is a legitimate situation, so we clear the breakpoint at the target address, and pass control back to the user
                core.clear_hw_breakpoint(0)?;
                (
                    core.status()?,
                    core.read_core_reg(core.program_counter().id())?
                        .try_into()?,
                )
            }
            Err(error) => {
                program_counter = core.halt(Duration::from_millis(500))?.pc;
                core.clear_hw_breakpoint(0)?;
                if matches!(
                    error,
                    crate::Error::Arm(ArmError::Timeout)
                        | crate::Error::Riscv(RiscvError::Timeout)
                        | crate::Error::Xtensa(XtensaError::Timeout)
                ) {
                    // This is not a quick step and halt operation. Notify the user that we are not going to wait any longer, and then return the current program counter so that the debugger can show the user where the forced halt happened.
                    tracing::error!(
                        "The core did not halt after stepping to {:#010X}. Forced a halt at {:#010X}. Long running operations between debug steps are not currently supported.",
                        target_address,
                        program_counter
                    );
                    (core.status()?, program_counter)
                } else {
                    // Something else is wrong.
                    return Err(DebugError::Other(format!(
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
/// - We reach the `target_address_range.end()`
/// - We reach an address that is not in the sequential range of `target_address_range`,
///     i.e. we stepped to some kind of branch instruction, or diversion to an interrupt handler.
/// - We reach some other legitimate halt point (e.g. the user tries to step past a series of statements,
///     but there is another breakpoint active in that "gap")
/// - We encounter an error (e.g. the core locks up).
fn step_to_address(
    target_address_range: RangeInclusive<u64>,
    core: &mut impl CoreInterface,
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
                other_halt_reason => return Err(DebugError::WarnAndContinue {
                    message: format!("Target halted unexpectedly before we reached the destination address of a step operation: {other_halt_reason:?}")
                }),
            },
            // This is not a recoverable error, and will result in the debug session ending (we have no predicatable way of successfully continuing the session)
            other_status => return Err(DebugError::Other(
                format!("Target failed to reach the destination address of a step operation: {:?}", other_status))
            ),
        }
    }
    Ok((
        core.status()?,
        core.read_core_reg(core.program_counter().id())?
            .try_into()?,
    ))
}
