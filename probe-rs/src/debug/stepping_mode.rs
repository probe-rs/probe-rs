use super::DebugError;
use super::DebugInfo;
use crate::core::Core;
use crate::CoreStatus;

/// Stepping granularity for stepping through a program during debug.
#[derive(Debug)]
pub enum SteppingMode {
    /// Advance one machine instruction at a time.
    StepInstruction,
    /// Step Over the current statement, and halt at the start of the next statement.
    OverStatement,
    /// DWARF doesn't encode when a statement contains a call to a non-inlined function.
    /// - The best-effort approach is to step a single instruction, and then find the first valid breakpoint address.
    /// - The worst case is that the user might have to perform the step into action more than once before the debugger properly steps into a function at the specified address.
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
    /// - Currently, no special provision is made for the effect of user defined breakpoints in interrupts that get triggered before this function completes.
    pub fn step(
        &self,
        core: &mut Core<'_>,
        debug_info: &DebugInfo,
    ) -> Result<(CoreStatus, u32), DebugError> {
        let mut core_status = core
            .status()
            .map_err(|error| DebugError::Other(anyhow::anyhow!(error)))
            .map_err(|error| DebugError::Other(anyhow::anyhow!(error)))?;
        let (mut program_counter, mut return_address) = match core_status {
            CoreStatus::Halted(_) => (
                core.read_core_reg(core.registers().program_counter())?,
                core.read_core_reg(core.registers().return_address())?,
            ),
            _ => {
                return Err(DebugError::Other(anyhow::anyhow!(
                    "Core must be halted before stepping."
                )))
            }
        };

        // First deal with the two special cases.
        match self {
            SteppingMode::StepInstruction => {
                program_counter = core.step()?.pc;
                core_status = core.status()?;
                return Ok((core_status, program_counter));
            }
            SteppingMode::IntoStatement => {
                // Step a single instruction, then proceed to the next step.
                program_counter = core.step()?.pc;
            }
            _ => {
                // We will deal with the rest in the next step.
            }
        }

        let mut target_address: Option<u64> = None;
        // Sometimes the target program_counter is at a location where the debug_info program row data does not contain valid statements for halt points.
        // When DebugError::NoValidHaltLocation happens, we will step to the next instruction and try again(until we can reasonably expect to have passed out of an epilogue), before giving up.
        for _ in 0..10 {
            match debug_info.get_halt_locations(program_counter as u64, Some(return_address as u64))
            {
                Ok(program_row_data) => {
                    match self {
                        SteppingMode::OverStatement => {
                            target_address = program_row_data.next_statement_address
                        }
                        SteppingMode::OutOfStatement => {
                            if program_row_data.step_out_address.is_none() {
                                return Err(DebugError::NoValidHaltLocation {
                                    message: "Cannot step out of a non-returning function"
                                        .to_string(),
                                    pc_at_error: program_counter as u64,
                                });
                            } else {
                                target_address = program_row_data.step_out_address
                            }
                        }
                        SteppingMode::IntoStatement => {
                            // We have already stepped a single instruction, now use the next available breakpoint.
                            target_address = program_row_data.first_halt_address
                        }
                        _ => {
                            // We've already covered SteppingMode::StepInstruction
                        }
                    }
                    // If we get here, we don't have to retry anymore.
                    break;
                }
                Err(error) => match error {
                    DebugError::NoValidHaltLocation {
                        message,
                        pc_at_error,
                    } => {
                        // Step on target instruction, and then try again.
                        log::debug!(
                            "Incomplete stepping information @{:#010X}: {}",
                            pc_at_error,
                            message
                        );
                        program_counter = core.step()?.pc;
                        return_address = core.read_core_reg(core.registers().return_address())?;
                        continue;
                    }
                    other_error => return Err(other_error),
                },
            }
        }
        match target_address {
            Some(target_address) => {
                log::debug!(
                    "Preparing to step ({:20?}) from: {:#010X} to: {:#010X}",
                    self,
                    program_counter,
                    target_address
                );

                if target_address == program_counter as u64 {
                    // For simple functions that complete in a single statement.
                    program_counter = core.step()?.pc;
                } else if core.set_hw_breakpoint(target_address as u32).is_ok() {
                    core.run()?;
                    core.clear_hw_breakpoint(target_address as u32)?;
                    core_status = match core.status() {
                        Ok(core_status) => {
                            match core_status {
                                CoreStatus::Halted(_) => {
                                    program_counter =
                                        core.read_core_reg(core.registers().program_counter())?
                                }
                                other => {
                                    log::error!(
                                        "Core should be halted after stepping but is: {:?}",
                                        &other
                                    );
                                    program_counter = 0;
                                }
                            };
                            core_status
                        }
                        Err(error) => return Err(DebugError::Probe(error)),
                    };
                } else {
                    while target_address != core.step()?.pc as u64 {
                        // Single step the core until we get to the target_address;
                        // TODO: In theory, this could go on for a long time. Should we consider NOT allowing this kind of stepping if there are no breakpoints available?
                    }
                    core_status = core.status()?;
                    program_counter = target_address as u32;
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
        Ok((core_status, program_counter))
    }
}
