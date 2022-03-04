use crate::dap_types;
use crate::debugger::ConsoleLog;
use crate::debugger::CoreData;
use crate::DebuggerError;
use anyhow::{anyhow, Result};
use dap_types::*;
use parse_int::parse;
use probe_rs::debug::Registers;
use probe_rs::debug::{VariableCache, VariableName};
use probe_rs::{debug::ColumnType, CoreStatus, HaltReason, MemoryInterface};
use probe_rs_cli_util::rtt;
use serde::{de::DeserializeOwned, Serialize};
use std::string::ToString;
use std::{
    convert::TryInto,
    path::{Path, PathBuf},
    str, thread,
    time::Duration,
};

use crate::protocol::ProtocolAdapter;

/// Progress ID used for progress reporting when the debug adapter protocol is used.
type ProgressId = i64;
pub struct DebugAdapter<P: ProtocolAdapter> {
    /// Track the last_known_status of the probe.
    /// The debug client needs to be notified when the probe changes state,
    /// and the only way is to poll the probe status periodically.
    /// For instance, when the client sets the probe running,
    /// and the probe halts because of a breakpoint, we need to notify the client.
    pub(crate) last_known_status: CoreStatus,
    pub(crate) halt_after_reset: bool,
    progress_id: ProgressId,
    /// Flag to indicate if the connected client supports progress reporting.
    pub(crate) supports_progress_reporting: bool,
    /// Flags to improve breakpoint accuracy.
    /// [DWARF] spec at Sect 2.14 uses 1 based numbering, with a 0 indicating not-specified. We will follow that standard, and translate incoming requests depending on the DAP Client treatment of 0 or 1 based numbering.
    pub(crate) lines_start_at_1: bool,
    /// [DWARF] spec at Sect 2.14 uses 1 based numbering, with a 0 indicating not-specified. We will follow that standard, and translate incoming requests depending on the DAP Client treatment of 0 or 1 based numbering.
    pub(crate) columns_start_at_1: bool,
    adapter: P,
}

impl<P: ProtocolAdapter> DebugAdapter<P> {
    pub fn new(adapter: P) -> DebugAdapter<P> {
        DebugAdapter {
            last_known_status: CoreStatus::Unknown,
            halt_after_reset: false,
            progress_id: 0,
            supports_progress_reporting: false,
            lines_start_at_1: true,
            columns_start_at_1: true,
            adapter,
        }
    }

    pub(crate) fn status(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        let status = match core_data.target_core.status() {
            Ok(status) => {
                self.last_known_status = status;
                status
            }
            Err(error) => {
                return self.send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "Could not read core status. {:?}",
                        error
                    ))),
                )
            }
        };
        if status.is_halted() {
            let pc = core_data
                .target_core
                .read_core_reg(core_data.target_core.registers().program_counter());
            match pc {
                Ok(pc) => self.send_response(
                    request,
                    Ok(Some(format!(
                        "Status: {:?} at address {:#010x}",
                        status.short_long_status().1,
                        pc
                    ))),
                ),

                Err(error) => self
                    .send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error)))),
            }
        } else {
            self.send_response(request, Ok(Some(status.short_long_status().1.to_string())))
        }
    }

    pub(crate) fn pause(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        // let args: PauseArguments = get_arguments(&request)?;

        match core_data.target_core.halt(Duration::from_millis(500)) {
            Ok(cpu_info) => {
                let event_body = Some(StoppedEventBody {
                    reason: "pause".to_owned(),
                    description: Some(self.last_known_status.short_long_status().1.to_owned()),
                    thread_id: Some(core_data.target_core.id() as i64),
                    preserve_focus_hint: Some(false),
                    text: None,
                    all_threads_stopped: Some(false), // TODO: Implement multi-core logic here
                    hit_breakpoint_ids: None,
                });
                self.send_event("stopped", event_body)?;
                self.send_response(
                    request,
                    Ok(Some(format!(
                        "Core stopped at address {:#010x}",
                        cpu_info.pc
                    ))),
                )?;
                self.last_known_status = CoreStatus::Halted(HaltReason::Request);

                Ok(())
            }
            Err(error) => {
                self.send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error))))
            }
        }

        // TODO: This is from original probe_rs_cli 'halt' function ... disasm code at memory location
        /*
        let mut code = [0u8; 16 * 2];

        core_data.target_core.read(cpu_info.pc, &mut code)?;

        let instructions = core_data
            .capstone
            .disasm_all(&code, u64::from(cpu_info.pc))
            .unwrap();

        for i in instructions.iter() {
            println!("{}", i);
        }


        for (offset, instruction) in code.iter().enumerate() {
            println!(
                "{:#010x}: {:010x}",
                cpu_info.pc + offset as u32,
                instruction
            );
        }
            */
    }

    pub(crate) fn read_memory(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        let arguments: ReadMemoryArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };
        let memory_offset = arguments.offset.unwrap_or(0);
        let mut address: u32 = if let Ok(address) =
            parse::<i64>(arguments.memory_reference.as_ref())
        {
            match (address + memory_offset).try_into() {
                    Ok(modified_address) => modified_address,
                    Err(error) => return self.send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "Could not convert memory_reference: {} and offset: {:?} into a 32-bit memory address: {:?}",
                        arguments.memory_reference, arguments.offset, error
                    ))),
                ),
                }
        } else {
            return self.send_response::<()>(
                request,
                Err(DebuggerError::Other(anyhow!(
                    "Could not read any data at address {:?}",
                    arguments.memory_reference
                ))),
            );
        };
        let mut num_bytes_unread = arguments.count as usize;
        let mut buff = vec![];
        while num_bytes_unread > 0 {
            if let Ok(good_byte) = core_data.target_core.read_word_8(address) {
                buff.push(good_byte);
                address += 1;
                num_bytes_unread -= 1;
            } else {
                break;
            }
        }
        if !buff.is_empty() || num_bytes_unread == 0 {
            let response = base64::encode(&buff);
            self.send_response(
                request,
                Ok(Some(ReadMemoryResponseBody {
                    address: format!("{:#010x}", address),
                    data: Some(response),
                    unreadable_bytes: None,
                })),
            )
        } else {
            self.send_response::<()>(
                request,
                Err(DebuggerError::Other(anyhow!(
                    "Could not read any data at address {:#010x}",
                    address
                ))),
            )
        }
    }

    pub(crate) fn write_memory(
        &mut self,
        core_data: &mut CoreData,
        request: Request,
    ) -> Result<()> {
        let arguments: WriteMemoryArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };
        let memory_offset = arguments.offset.unwrap_or(0);
        let address: u32 = if let Ok(address) = parse::<i64>(arguments.memory_reference.as_ref()) {
            match (address + memory_offset).try_into() {
                    Ok(modified_address) => modified_address,
                    Err(error) => return self.send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "Could not convert memory_reference: {} and offset: {:?} into a 32-bit memory address: {:?}",
                        arguments.memory_reference, arguments.offset, error
                    ))),
                ),
                }
        } else {
            return self.send_response::<()>(
                request,
                Err(DebuggerError::Other(anyhow!(
                    "Could not read any data at address {:?}",
                    arguments.memory_reference
                ))),
            );
        };
        let data_bytes = match base64::decode(&arguments.data) {
            Ok(decoded_bytes) => decoded_bytes,
            Err(error) => {
                return self.send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "Could not decode base64 data:{:?} :  {:?}",
                        arguments.data,
                        error
                    ))),
                );
            }
        };
        match core_data
            .target_core
            .write_8(address, &data_bytes)
            .map_err(DebuggerError::ProbeRs)
        {
            Ok(_) => {
                self.send_response(
                    request,
                    Ok(Some(WriteMemoryResponseBody {
                        bytes_written: Some(data_bytes.len() as i64),
                        offset: None,
                    })),
                )?;
                // TODO: This doesn't trigger the UI to reload the variables effected. Investigate if we can force it in some other way, or if it is a known issue.
                self.send_event(
                    "memory",
                    Some(MemoryEventBody {
                        count: data_bytes.len() as i64,
                        memory_reference: format!("{:#010x}", address),
                        offset: 0,
                    }),
                )
            }
            Err(error) => self.send_response::<()>(request, Err(error)),
        }
    }

    /// Evaluates the given expression in the context of the top most stack frame.
    /// The expression has access to any variables and arguments that are in scope.
    pub(crate) fn evaluate(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        // TODO: When variables appear in the `watch` context, they will not resolve correctly after a 'step' function. Consider doing the lazy load for 'either/or' of Variables vs. Evaluate

        let arguments: EvaluateArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };

        // Various fields in the response_body will be updated before we return.
        let mut response_body = EvaluateResponseBody {
            indexed_variables: None,
            memory_reference: None,
            named_variables: None,
            presentation_hint: None,
            result: format!("<variable not found {:?}>", arguments.expression),
            type_: None,
            variables_reference: 0_i64,
        };

        // The Variables request always returns a 'evaluate_name' = 'name', this means that the expression will always be the variable name we are looking for.
        let expression = arguments.expression.clone();

        // Make sure we have a valid StackFrame
        if let Some(stack_frame) = match arguments.frame_id {
            Some(frame_id) => core_data
                .stack_frames
                .iter_mut()
                .find(|stack_frame| stack_frame.id == frame_id),
            None => {
                // Use the current frame_id
                core_data.stack_frames.first_mut()
            }
        } {
            // Always search the registers first, because we don't have a VariableCache for them.
            if let Some((_register_number, register_value)) =
                stack_frame.registers.registers().into_iter().find(
                    |(register_number, _register_value)| {
                        let register_number = **register_number;
                        let register_name = stack_frame
                            .registers
                            .get_name_by_dwarf_register_number(register_number)
                            .unwrap_or_else(|| format!("r{:#}", register_number));
                        register_name == expression
                    },
                )
            {
                response_body.type_ = Some(format!("{}", VariableName::RegistersRoot));
                response_body.result = format!("{:#010x}", register_value);
            } else {
                // If the expression wasn't pointing to a register, then check if is a local or static variable in our stack_frame
                let mut variable: Option<probe_rs::debug::Variable> = None;
                let mut variable_cache: Option<&mut VariableCache> = None;
                // Search through available caches and stop as soon as the variable is found
                #[allow(clippy::manual_flatten)]
                for stack_frame_variable_cache in [
                    stack_frame.local_variables.as_mut(),
                    stack_frame.static_variables.as_mut(),
                ] {
                    if let Some(search_cache) = stack_frame_variable_cache {
                        variable = search_cache
                            .get_variable_by_name(&VariableName::Named(expression.clone()));
                        if variable.is_some() {
                            variable_cache = Some(search_cache);
                            break;
                        }
                    }
                }
                // Check if we found a variable.
                if let (Some(variable), Some(variable_cache)) = (variable, variable_cache) {
                    let (
                        variables_reference,
                        named_child_variables_cnt,
                        indexed_child_variables_cnt,
                    ) = self.get_variable_reference(&variable, variable_cache);
                    response_body.indexed_variables = Some(indexed_child_variables_cnt);
                    response_body.memory_reference =
                        Some(format!("{:#010x}", variable.memory_location));
                    response_body.named_variables = Some(named_child_variables_cnt);
                    response_body.result = variable.get_value(variable_cache);
                    response_body.type_ = Some(variable.type_name.clone());
                    response_body.variables_reference = variables_reference;
                } else {
                    // If we made it to here, no register or variable matched the expression.
                }
            }
        }

        self.send_response(request, Ok(Some(response_body)))
    }

    /// Set the variable with the given name in the variable container to a new value.
    pub(crate) fn set_variable(
        &mut self,
        core_data: &mut CoreData,
        request: Request,
    ) -> Result<()> {
        let arguments: SetVariableArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };

        // Various fields in the response_body will be updated before we return.
        let mut response_body = SetVariableResponseBody {
            indexed_variables: None,
            named_variables: None,
            type_: None,
            value: String::new(),
            variables_reference: None,
        };

        // The arguments.variables_reference contains the reference of the variable container. This can be:
        // - The `StackFrame.id` for register variables - we will warn the user that updating these are not yet supported.
        // - The `Variable.parent_key` for a local or static variable - If these are base data types, we will attempt to update their value, otherwise we will warn the user that updating complex / structure variables are not yet supported.
        let parent_key = arguments.variables_reference;
        let new_value = arguments.value.clone();

        match core_data
            .stack_frames
            .iter_mut()
            .find(|stack_frame| stack_frame.id == parent_key)
        {
            Some(stack_frame) => {
                // The variable is a register value in this StackFrame
                if let Some((_register_number, _register_value)) =
                    stack_frame.registers.registers().into_iter().find(
                        |(register_number, _register_value)| {
                            let register_number = **register_number;
                            let register_name = stack_frame
                                .registers
                                .get_name_by_dwarf_register_number(register_number)
                                .unwrap_or_else(|| format!("r{:#}", register_number));
                            register_name == arguments.name
                        },
                    )
                {
                    // TODO: Does it make sense for us to consider implementing an update of platform registers?
                    return self.send_response::<SetVariableResponseBody>(
                        request,
                        Err(DebuggerError::Other(anyhow!(
                            "Set Register values is not yet supported."
                        ))),
                    );
                }
            }
            None => {
                let variable_name = VariableName::Named(arguments.name.clone());

                // The parent_key refers to a local or static variable in one of the in-scope StackFrames.
                let mut cache_variable: Option<probe_rs::debug::Variable> = None;
                let mut variable_cache: Option<&mut VariableCache> = None;
                for search_frame in core_data.stack_frames.iter_mut() {
                    if let Some(search_cache) = &mut search_frame.local_variables {
                        if let Some(search_variable) = search_cache
                            .get_variable_by_name_and_parent(&variable_name, Some(parent_key))
                        {
                            cache_variable = Some(search_variable);
                            variable_cache = Some(search_cache);
                            break;
                        }
                    }
                    if let Some(search_cache) = &mut search_frame.static_variables {
                        if let Some(search_variable) = search_cache
                            .get_variable_by_name_and_parent(&variable_name, Some(parent_key))
                        {
                            cache_variable = Some(search_variable);
                            variable_cache = Some(search_cache);
                            break;
                        }
                    }
                }

                if let (Some(cache_variable), Some(variable_cache)) =
                    (cache_variable, variable_cache)
                {
                    // We have found the variable that needs to be updated.
                    match cache_variable.update_value(
                        &mut core_data.target_core,
                        variable_cache,
                        new_value.clone(),
                    ) {
                        Ok(updated_value) => {
                            let (
                                variables_reference,
                                named_child_variables_cnt,
                                indexed_child_variables_cnt,
                            ) = self.get_variable_reference(&cache_variable, variable_cache);
                            response_body.variables_reference = Some(variables_reference);
                            response_body.named_variables = Some(named_child_variables_cnt);
                            response_body.indexed_variables = Some(indexed_child_variables_cnt);
                            response_body.type_ = Some(cache_variable.type_name);
                            response_body.value = updated_value;
                        }
                        Err(error) => {
                            return self.send_response::<SetVariableResponseBody>(
                                request,
                                Err(DebuggerError::Other(anyhow!(
                                    "Failed to update variable: {}, with new value {:?} : {:?}",
                                    cache_variable.name,
                                    new_value,
                                    error
                                ))),
                            );
                        }
                    }
                }
            }
        }

        if response_body.value.is_empty() {
            // If we get here, it is a bug.
            self.send_response::<SetVariableResponseBody>(
                                request,
                                Err(DebuggerError::Other(anyhow!(
                                    "Failed to update variable: {}, with new value {:?} : Please report this as a bug.",
                                    arguments.name,
                                    arguments.value
                                ))),
                            )
        } else {
            self.send_response(request, Ok(Some(response_body)))
        }
    }

    pub(crate) fn set_breakpoint(
        &mut self,
        core_data: &mut CoreData,
        request: Request,
    ) -> Result<()> {
        let address = match get_int_argument(request.arguments.as_ref(), "address", 0) {
            Ok(address) => address,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };

        match core_data
            .target_core
            .set_hw_breakpoint(address)
            .map_err(DebuggerError::ProbeRs)
        {
            Ok(_) => {
                return self.send_response(
                    request,
                    Ok(Some(format!(
                        "Set new breakpoint at address {:#08x}",
                        address
                    ))),
                );
            }
            Err(error) => self.send_response::<()>(request, Err(error)),
        }
    }
    pub(crate) fn clear_breakpoint(
        &mut self,
        core_data: &mut CoreData,
        request: Request,
    ) -> Result<()> {
        let address = match get_int_argument(request.arguments.as_ref(), "address", 0) {
            Ok(address) => address,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };

        match core_data
            .target_core
            .clear_hw_breakpoint(address)
            .map_err(DebuggerError::ProbeRs)
        {
            Ok(_) => Ok(()),
            Err(error) => self.send_response::<()>(request, Err(error)),
        }
    }

    pub(crate) fn restart(
        &mut self,
        core_data: &mut CoreData,
        request: Option<Request>,
    ) -> Result<()> {
        match core_data.target_core.halt(Duration::from_millis(500)) {
            Ok(_) => {}
            Err(error) => {
                if let Some(request) = request {
                    return self.send_response::<()>(
                        request,
                        Err(DebuggerError::Other(anyhow!("{}", error))),
                    );
                } else {
                    return self.send_error_response(&DebuggerError::Other(anyhow!("{}", error)));
                }
            }
        }

        // Different code paths if we invoke this from a request, versus an internal function.
        if request.is_some() {
            match core_data.target_core.reset() {
                Ok(_) => {
                    self.last_known_status = CoreStatus::Running;
                    let event_body = Some(ContinuedEventBody {
                        all_threads_continued: Some(false), // TODO: Implement multi-core logic here
                        thread_id: core_data.target_core.id() as i64,
                    });

                    self.send_event("continued", event_body)
                }
                Err(error) => {
                    return self.send_response::<()>(
                        request.unwrap(), // Checked above
                        Err(DebuggerError::Other(anyhow!("{}", error))),
                    );
                }
            }
        } else
        // The DAP Client will always do a `reset_and_halt`, and then will consider `halt_after_reset` value after the `configuration_done` request.
        // Otherwise the probe will run past the `main()` before the DAP Client has had a chance to set breakpoints in `main()`.
        {
            match core_data
                .target_core
                .reset_and_halt(Duration::from_millis(500))
            {
                Ok(_) => {
                    if let Some(request) = request {
                        return self.send_response::<()>(request, Ok(None));
                    }
                    // Only notify the DAP client if we are NOT in initialization stage (`CoreStatus::Unknown`).
                    if self.last_known_status != CoreStatus::Unknown {
                        let event_body = Some(StoppedEventBody {
                            reason: "reset".to_owned(),
                            description: Some(
                                CoreStatus::Halted(HaltReason::External)
                                    .short_long_status()
                                    .1
                                    .to_string(),
                            ),
                            thread_id: Some(core_data.target_core.id() as i64),
                            preserve_focus_hint: None,
                            text: None,
                            all_threads_stopped: Some(false), // TODO: Implement multi-core logic here
                            hit_breakpoint_ids: None,
                        });
                        self.send_event("stopped", event_body)?;
                        self.last_known_status = CoreStatus::Halted(HaltReason::External);
                    }
                    Ok(())
                }
                Err(error) => {
                    if let Some(request) = request {
                        return self.send_response::<()>(
                            request,
                            Err(DebuggerError::Other(anyhow!("{}", error))),
                        );
                    } else {
                        return self
                            .send_error_response(&DebuggerError::Other(anyhow!("{}", error)));
                    }
                }
            }
        }
    }

    /// NOTE: VSCode sends a 'threads' request when it receives the response from this request, irrespective of target state.
    /// This can lead to duplicate `threads->stacktrace->etc.` sequences if & when the target halts and sends a 'stopped' event.
    /// See [https://github.com/golang/vscode-go/issues/940] for more info.
    /// In order to avoid overhead and duplicate responses, we will implement the following logic.
    /// - `configuration_done` will ignore target status, and simply notify VSCode that we're done.
    /// - `threads` will check for [DebugAdapter::last_known_status] and ...
    ///   - If it is `Unknown`, it will ...
    ///     - send back a threads response, with `all_threds_stopped=Some(false)`
    ///     - check on actual core status, and update [DebugAdapter::last_known_status] as well as synch status with the VSCode client.
    ///   - If it is `Halted`, it will respond with thread information as expected.
    ///   - Any other status will send and error.
    pub(crate) fn configuration_done(
        &mut self,
        _core_data: &mut CoreData,
        request: Request,
    ) -> Result<()> {
        self.send_response::<()>(request, Ok(None))
    }

    pub(crate) fn threads(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        // TODO: Implement actual thread resolution. For now, we just use the core id as the thread id.
        let mut threads: Vec<Thread> = vec![];
        match self.last_known_status {
            CoreStatus::Unknown => {
                // We are probably here because the `configuration_done` request just happened, so we can make sure the client and debugger are in synch.
                match core_data.target_core.status() {
                    Ok(core_status) => {
                        self.last_known_status = core_status;
                        // Make sure the DAP Client and the DAP Server are in sync with the status of the core.
                        if core_status.is_halted() {
                            if self.halt_after_reset
                                || core_status == CoreStatus::Halted(HaltReason::Breakpoint)
                            {
                                let event_body = Some(StoppedEventBody {
                                    reason: core_status.short_long_status().0.to_owned(),
                                    description: Some(
                                        core_status.short_long_status().1.to_string(),
                                    ),
                                    thread_id: Some(core_data.target_core.id() as i64),
                                    preserve_focus_hint: None,
                                    text: None,
                                    all_threads_stopped: Some(false), // TODO: Implement multi-core logic here
                                    hit_breakpoint_ids: None,
                                });
                                self.send_event("stopped", event_body)?;
                            } else {
                                let single_thread = Thread {
                                    id: core_data.target_core.id() as i64,
                                    name: core_data.target_name.clone(),
                                };
                                threads.push(single_thread);
                                self.send_response(
                                    request.clone(),
                                    Ok(Some(ThreadsResponseBody { threads })),
                                )?;
                                return self.r#continue(core_data, request);
                            }
                        }
                    }
                    Err(error) => {
                        return self.send_response::<()>(
                            request,
                            Err(DebuggerError::Other(anyhow!(
                                "Could not read core status to synchronize the client and the probe. {:?}",
                                error
                            ))),
                        );
                    }
                }
            }
            CoreStatus::Halted(_) => {
                let single_thread = Thread {
                    id: core_data.target_core.id() as i64,
                    name: core_data.target_name.clone(),
                };
                threads.push(single_thread);
            }
            CoreStatus::Running | CoreStatus::LockedUp | CoreStatus::Sleeping => {
                return self.send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "Received request for `threads`, while last known core status was {:?}",
                        self.last_known_status
                    ))),
                );
            }
        }
        self.send_response(request, Ok(Some(ThreadsResponseBody { threads })))
    }

    pub(crate) fn set_breakpoints(
        &mut self,
        core_data: &mut CoreData,
        request: Request,
    ) -> Result<()> {
        let args: SetBreakpointsArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => {
                return self.send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "Could not read arguments : {}",
                        error
                    ))),
                )
            }
        };

        let mut created_breakpoints: Vec<Breakpoint> = Vec::new(); // For returning in the Response

        let source_path = args.source.path.as_ref().map(Path::new);

        // Always clear existing breakpoints before setting new ones. The DAP Specification doesn't make allowances for deleting and setting individual breakpoints.
        match core_data.target_core.clear_all_hw_breakpoints() {
            Ok(_) => {}
            Err(error) => {
                return self.send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "Failed to clear existing breakpoints before setting new ones : {}",
                        error
                    ))),
                )
            }
        }

        if let Some(requested_breakpoints) = args.breakpoints.as_ref() {
            for bp in requested_breakpoints {
                // Some overrides to improve breakpoint accuracy when `DebugInfo::get_breakpoint_location()` has to select the best from multiple options
                let breakpoint_line = if self.lines_start_at_1 {
                    // If the debug client uses 1 based numbering, then we can use it as is.
                    bp.line as u64
                } else {
                    // If the debug client uses 0 based numbering, then we bump the number by 1
                    bp.line as u64 + 1
                };
                let breakpoint_column = if self.columns_start_at_1
                    && (bp.column.is_none() || bp.column.unwrap_or(0) == 0)
                {
                    // If the debug client uses 1 based numbering, then we can use it as is.
                    Some(bp.column.unwrap_or(1) as u64)
                } else {
                    // If the debug client uses 0 based numbering, then we bump the number by 1
                    Some(bp.column.unwrap_or(0) as u64 + 1)
                };

                // Try to find source code location
                let source_location: Option<u64> = core_data
                    .debug_info
                    .get_breakpoint_location(
                        source_path.unwrap(),
                        breakpoint_line,
                        breakpoint_column,
                    )
                    .unwrap_or(None);

                if let Some(location) = source_location {
                    let (verified, reason_msg) =
                        match core_data.target_core.set_hw_breakpoint(location as u32) {
                            Ok(_) => (
                                true,
                                Some(format!("Breakpoint at memory address: {:#010x}", location)),
                            ),
                            Err(err) => {
                                let message = format!(
                                "WARNING: Could not set breakpoint at memory address: {:#010x}: {}",
                                location, err
                            )
                                .to_string();
                                // In addition to sending the error to the 'Hover' message, also write it to the Debug Console Log.
                                self.log_to_console(format!("WARNING: {}", message));
                                self.show_message(MessageSeverity::Warning, message.clone());
                                (false, Some(message))
                            }
                        };

                    created_breakpoints.push(Breakpoint {
                        column: breakpoint_column.map(|c| c as i64),
                        end_column: None,
                        end_line: None,
                        id: None,
                        line: Some(breakpoint_line as i64),
                        message: reason_msg,
                        source: None,
                        instruction_reference: Some(location.to_string()),
                        offset: None,
                        verified,
                    });
                } else {
                    let message = "No source location for breakpoint. Try reducing `opt-level` in `Cargo.toml` ".to_string();
                    // In addition to sending the error to the 'Hover' message, also write it to the Debug Console Log.
                    self.log_to_console(format!("WARNING: {}", message));
                    self.show_message(MessageSeverity::Warning, message.clone());
                    created_breakpoints.push(Breakpoint {
                        column: bp.column,
                        end_column: None,
                        end_line: None,
                        id: None,
                        line: Some(bp.line),
                        message: Some(message),
                        source: None,
                        instruction_reference: None,
                        offset: None,
                        verified: false,
                    });
                }
            }
        }

        let breakpoint_body = SetBreakpointsResponseBody {
            breakpoints: created_breakpoints,
        };
        self.send_response(request, Ok(Some(breakpoint_body)))
    }

    pub(crate) fn stack_trace(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        let _status = match core_data.target_core.status() {
            Ok(status) => {
                if !status.is_halted() {
                    return self.send_response::<()>(
                        request,
                        Err(DebuggerError::Other(anyhow!(
                            "Core must be halted before requesting a stack trace"
                        ))),
                    );
                }
            }
            Err(error) => {
                return self.send_response::<()>(request, Err(DebuggerError::ProbeRs(error)))
            }
        };

        let arguments: StackTraceArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => {
                return self.send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "Could not read arguments : {}",
                        error
                    ))),
                )
            }
        };

        let regs = core_data.target_core.registers();

        let pc = match core_data.target_core.read_core_reg(regs.program_counter()) {
            Ok(pc) => pc,
            Err(error) => {
                return self.send_response::<()>(request, Err(DebuggerError::ProbeRs(error)))
            }
        };

        log::debug!(
            "Updating the stack frame data for core #{}",
            core_data.target_core.id()
        );

        if let Some(levels) = arguments.levels {
            if let Some(start_frame) = arguments.start_frame {
                if levels == 20 && start_frame == 0 {
                    // This is a invalid stack_trace from VSCode, so let's respond in kind.
                    let body = StackTraceResponseBody {
                        stack_frames: vec![],
                        total_frames: Some(0i64),
                    };
                    return self.send_response(request, Ok(Some(body)));
                } else if levels == 1 && start_frame == 0 {
                    // This is a legit request for the first frame in a new stack_trace, so do a new unwind.
                    *core_data.stack_frames = core_data
                        .debug_info
                        .unwind(&mut core_data.target_core, u64::from(pc))?;
                }
                // Determine the correct 'slice' of available [StackFrame]s to serve up ...
                let total_frames = core_data.stack_frames.len() as i64;
                let frame_slice = if levels == 1 && start_frame == 0 {
                    // Just the first frame - use the LHS of the split at `levels`
                    core_data.stack_frames.split_at(levels as usize).0.iter()
                } else if total_frames <= 20 && start_frame >= 0 && start_frame <= total_frames {
                    // When we have less than 20 frames - use the RHS of of the split at `start_frame`
                    core_data
                        .stack_frames
                        .split_at(start_frame as usize)
                        .1
                        .iter()
                } else if total_frames > 20 && start_frame + levels <= total_frames {
                    // When we have more than 20 frames - we can safely split twice
                    core_data
                        .stack_frames
                        .split_at(start_frame as usize)
                        .1
                        .split_at(levels as usize)
                        .0
                        .iter()
                } else {
                    return self.send_response::<()>(
                        request,
                        Err(DebuggerError::Other(anyhow!(
                            "Request for stack trace failed with invalid arguments: {:?}",
                            arguments
                        ))),
                    );
                };

                let frame_list: Vec<StackFrame> = frame_slice
                    .map(|frame| {
                        let column = frame
                            .source_location
                            .as_ref()
                            .and_then(|sl| sl.column)
                            .map(|col| match col {
                                ColumnType::LeftEdge => 0,
                                ColumnType::Column(c) => c,
                            })
                            .unwrap_or(0);

                        let source = if let Some(source_location) = &frame.source_location {
                            let path: Option<PathBuf> =
                                source_location.directory.as_ref().map(|path| {
                                    let mut path = if path.is_relative() {
                                        std::env::current_dir().unwrap().join(path)
                                    } else {
                                        path.to_owned()
                                    };

                                    if let Some(file) = &source_location.file {
                                        path.push(file);
                                    }

                                    path
                                });
                            Some(Source {
                                name: source_location.file.clone(),
                                path: path.map(|p| p.to_string_lossy().to_string()),
                                source_reference: None,
                                presentation_hint: None,
                                origin: None,
                                sources: None,
                                adapter_data: None,
                                checksums: None,
                            })
                        } else {
                            log::debug!("No source location present for frame!");
                            None
                        };

                        let line = frame
                            .source_location
                            .as_ref()
                            .and_then(|sl| sl.line)
                            .unwrap_or(0) as i64;
                        let function_display_name = if frame.is_inlined {
                            format!("{} #[inline]", frame.function_name)
                        } else {
                            format!("{} @{:#010x}", frame.function_name, frame.pc)
                        };
                        // TODO: Can we add more meaningful info to `module_id`, etc.
                        StackFrame {
                            id: frame.id as i64,
                            name: function_display_name,
                            source,
                            line,
                            column: column as i64,
                            end_column: None,
                            end_line: None,
                            module_id: None,
                            presentation_hint: Some("normal".to_owned()),
                            can_restart: Some(false),
                            instruction_pointer_reference: Some(format!("{:#010x}", frame.pc)),
                        }
                    })
                    .collect();

                let body = StackTraceResponseBody {
                    stack_frames: frame_list,
                    total_frames: Some(total_frames),
                };
                self.send_response(request, Ok(Some(body)))
            } else {
                self.send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "Request for stack trace failed with invalid start_frame argument: {:?}",
                        arguments.start_frame
                    ))),
                )
            }
        } else {
            self.send_response::<()>(
                request,
                Err(DebuggerError::Other(anyhow!(
                    "Request for stack trace failed with invalid levels argument: {:?}",
                    arguments.levels
                ))),
            )
        }
    }

    /// Retrieve available scopes  
    /// - static scope  : Variables with `static` modifier
    /// - registers     : The [probe_rs::Core::registers] for the target [probe_rs::CoreType]
    /// - local scope   : Variables defined between start of current frame, and the current pc (program counter)
    pub(crate) fn scopes(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        let arguments: ScopesArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };

        let mut dap_scopes: Vec<Scope> = vec![];

        log::trace!("Getting scopes for frame {}", arguments.frame_id,);

        if let Some(stack_frame) = core_data.get_stackframe(arguments.frame_id) {
            if let Some(static_root_variable) =
                stack_frame
                    .static_variables
                    .as_ref()
                    .and_then(|stack_frame| {
                        stack_frame
                            .get_variable_by_name_and_parent(&VariableName::StaticScopeRoot, None)
                    })
            {
                dap_scopes.push(Scope {
                    line: None,
                    column: None,
                    end_column: None,
                    end_line: None,
                    expensive: true, // VSCode won't open this tree by default.
                    indexed_variables: None,
                    name: "Static".to_string(),
                    presentation_hint: Some("statics".to_string()),
                    named_variables: None,
                    source: None,
                    variables_reference: static_root_variable.variable_key,
                });
            };

            dap_scopes.push(Scope {
                line: None,
                column: None,
                end_column: None,
                end_line: None,
                expensive: true, // VSCode won't open this tree by default.
                indexed_variables: None,
                name: "Registers".to_string(),
                presentation_hint: Some("registers".to_string()),
                named_variables: None,
                source: None,
                // We use the stack_frame.id for registers, so that we don't need to cache copies of the registers.
                variables_reference: stack_frame.id,
            });

            if let Some(locals_root_variable) =
                stack_frame
                    .local_variables
                    .as_ref()
                    .and_then(|stack_frame| {
                        stack_frame
                            .get_variable_by_name_and_parent(&VariableName::LocalScopeRoot, None)
                    })
            {
                dap_scopes.push(Scope {
                    line: stack_frame
                        .source_location
                        .as_ref()
                        .and_then(|location| location.line.map(|line| line as i64)),
                    column: stack_frame.source_location.as_ref().and_then(|l| {
                        l.column.map(|c| match c {
                            ColumnType::LeftEdge => 0,
                            ColumnType::Column(c) => c as i64,
                        })
                    }),
                    end_column: None,
                    end_line: None,
                    expensive: false, // VSCode will open this tree by default.
                    indexed_variables: None,
                    name: "Variables".to_string(),
                    presentation_hint: Some("locals".to_string()),
                    named_variables: None,
                    source: None,
                    variables_reference: locals_root_variable.variable_key,
                });
            };
        }

        self.send_response(request, Ok(Some(ScopesResponseBody { scopes: dap_scopes })))
    }

    pub(crate) fn source(&mut self, _core_data: &mut CoreData, request: Request) -> Result<()> {
        let arguments: SourceArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };

        let result = if let Some(path) = arguments.source.and_then(|s| s.path) {
            let mut source_path = PathBuf::from(path);

            if source_path.is_relative() {
                source_path = std::env::current_dir().unwrap().join(source_path);
            }
            match std::fs::read_to_string(&source_path) {
                Ok(source_code) => Ok(Some(SourceResponseBody {
                    content: source_code,
                    mime_type: None,
                })),
                Err(error) => {
                    return self.send_response::<()>(
                        request,
                        Err(DebuggerError::ReadSourceError {
                            source_file_name: (&source_path.to_string_lossy()).to_string(),
                            original_error: error,
                        }),
                    )
                }
            }
        } else {
            return self.send_response::<()>(
                request,
                Err(DebuggerError::Other(anyhow!("Unable to open resource"))),
            );
        };

        self.send_response(request, result)
    }

    pub(crate) fn variables(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        let arguments: VariablesArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };

        let response = {
            // The MS DAP Specification only gives us the unique reference of the variable, and does not tell us which StackFrame it belongs to, nor does it specify if this variable is in the local, register or static scope. Unfortunately this means we have to search through all the available [VariableCache]'s until we find it. To minimize the impact of this, we will search in the most 'likely' places first (first stack frame's locals, then statics, then registers, then move to next stack frame, and so on ...)
            let mut parent_variable: Option<probe_rs::debug::Variable> = None;
            let mut variable_cache: Option<&mut VariableCache> = None;
            let mut stack_frame_registers: Option<&Registers> = None;
            for stack_frame in core_data.stack_frames.iter_mut() {
                if let Some(search_cache) = &mut stack_frame.local_variables {
                    if let Some(search_variable) =
                        search_cache.get_variable_by_key(arguments.variables_reference)
                    {
                        parent_variable = Some(search_variable);
                        variable_cache = Some(search_cache);
                        stack_frame_registers = Some(&stack_frame.registers);
                        break;
                    }
                }
                if let Some(search_cache) = &mut stack_frame.static_variables {
                    if let Some(search_variable) =
                        search_cache.get_variable_by_key(arguments.variables_reference)
                    {
                        parent_variable = Some(search_variable);
                        variable_cache = Some(search_cache);
                        stack_frame_registers = Some(&stack_frame.registers);
                        break;
                    }
                }

                if stack_frame.id == arguments.variables_reference {
                    // This is a special case, where we just want to return the stack frame registers.

                    let mut sorted_registers = stack_frame
                        .registers
                        .registers()
                        .collect::<Vec<(&u32, &u32)>>();
                    sorted_registers
                        .sort_by_key(|(register_number, _register_value)| *register_number);

                    let dap_variables: Vec<Variable> = sorted_registers
                        .iter()
                        .map(|(&register_number, &register_value)| Variable {
                            name: stack_frame
                                .registers
                                .get_name_by_dwarf_register_number(register_number)
                                .unwrap_or_else(|| format!("r{:#}", register_number)),
                            evaluate_name: Some(
                                stack_frame
                                    .registers
                                    .get_name_by_dwarf_register_number(register_number)
                                    .unwrap_or_else(|| format!("r{:#}", register_number)),
                            ),
                            memory_reference: None,
                            indexed_variables: None,
                            named_variables: None,
                            presentation_hint: None, // TODO: Implement hint as Hex for registers
                            type_: Some(format!("{}", VariableName::RegistersRoot)),
                            value: format!("{:#010x}", register_value),
                            variables_reference: 0,
                        })
                        .collect();
                    return self.send_response(
                        request,
                        Ok(Some(VariablesResponseBody {
                            variables: dap_variables,
                        })),
                    );
                }
            }

            // During the intial stack unwind operation, if encounter [Variable]'s with [VariableNodeType::is_deferred()], they will not be auto-expanded and included in the variable cache.
            // TODO: Use the DAP "Invalidated" event to refresh the variables for this stackframe. It will allow the UI to see updated compound values for pointer variables based on the newly resolved children.
            if let Some(variable_cache) = variable_cache {
                if let Some(parent_variable) = parent_variable.as_mut() {
                    if parent_variable.variable_node_type.is_deferred()
                        && !variable_cache.has_children(parent_variable)?
                    {
                        if let Some(stack_frame_registers) = stack_frame_registers {
                            core_data.debug_info.cache_deferred_variables(
                                variable_cache,
                                &mut core_data.target_core,
                                parent_variable,
                                stack_frame_registers,
                            )?;
                        } else {
                            log::error!("Could not cache deferred child variables for variable: {}. No register data available.", parent_variable.name );
                        }
                    }
                }

                let dap_variables: Vec<Variable> = variable_cache
                    .get_children(Some(arguments.variables_reference))?
                    .iter()
                    // Filter out requested children, then map them as DAP variables
                    .filter(|variable| match &arguments.filter {
                        Some(filter) => match filter.as_str() {
                            "indexed" => variable.is_indexed(),
                            "named" => !variable.is_indexed(),
                            other => {
                                // This will yield an empty Vec, which will result in a user facing error as well as the log below.
                                log::error!("Received invalid variable filter: {}", other);
                                false
                            }
                        },
                        None => true,
                    })
                    // Convert the `probe_rs::debug::Variable` to `probe_rs_debugger::dap_types::Variable`
                    .map(|variable| {
                        let (
                            variables_reference,
                            named_child_variables_cnt,
                            indexed_child_variables_cnt,
                        ) = self.get_variable_reference(variable, variable_cache);
                        Variable {
                            name: variable.name.to_string(),
                            evaluate_name: Some(variable.name.to_string()),
                            memory_reference: Some(format!("{:#010x}", variable.memory_location)),
                            indexed_variables: Some(indexed_child_variables_cnt),
                            named_variables: Some(named_child_variables_cnt),
                            presentation_hint: None,
                            type_: Some(variable.type_name.clone()),
                            value: variable.get_value(variable_cache),
                            variables_reference,
                        }
                    })
                    .collect();
                Ok(Some(VariablesResponseBody {
                    variables: dap_variables,
                }))
            } else {
                Err(DebuggerError::Other(anyhow!(
                    "No variable information found for {}!",
                    arguments.variables_reference
                )))
            }
        };

        self.send_response(request, response)
    }

    pub(crate) fn r#continue(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        match core_data.target_core.run() {
            Ok(_) => {
                self.last_known_status = core_data
                    .target_core
                    .status()
                    .unwrap_or(CoreStatus::Unknown);
                if request.command.as_str() == "continue" {
                    // If this continue was initiated as part of some other request, then do not respond.
                    self.send_response(
                        request,
                        Ok(Some(ContinueResponseBody {
                            all_threads_continued: Some(false), // TODO: Implement multi-core logic here
                        })),
                    )?;
                }
                // We have to consider the fact that sometimes the `run()` is successfull,
                // but "immediately" afterwards, the MCU hits a breakpoint or exception.
                // So we have to check the status again to be sure.
                thread::sleep(Duration::from_millis(100)); // Small delay to make sure the MCU hits user breakpoints early in `main()`.
                let core_status = match core_data.target_core.status() {
                    Ok(new_status) => match new_status {
                        CoreStatus::Halted(_) => {
                            let event_body = Some(StoppedEventBody {
                                reason: new_status.short_long_status().0.to_owned(),
                                description: Some(new_status.short_long_status().1.to_string()),
                                thread_id: Some(core_data.target_core.id() as i64),
                                preserve_focus_hint: None,
                                text: None,
                                all_threads_stopped: Some(false), // TODO: Implement multi-core logic here
                                hit_breakpoint_ids: None,
                            });
                            self.send_event("stopped", event_body)?;
                            new_status
                        }
                        other => other,
                    },
                    Err(_) => CoreStatus::Unknown,
                };
                self.last_known_status = core_status;
                Ok(())
            }
            Err(error) => {
                self.last_known_status = CoreStatus::Halted(HaltReason::Unknown);
                self.send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error))))?;
                Err(error.into())
            }
        }
    }

    /// Steps at 'instruction' granularity ONLY.
    pub(crate) fn next(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        // TODO: Implement 'statement' granularity, then update DAP `Capabilities` and read `NextArguments`.
        // let args: NextArguments = get_arguments(&request)?;

        match core_data.target_core.step() {
            Ok(cpu_info) => {
                let new_status = match core_data.target_core.status() {
                    Ok(new_status) => new_status,
                    Err(error) => {
                        self.send_response::<()>(request, Err(DebuggerError::ProbeRs(error)))?;
                        return Err(anyhow!("Failed to retrieve core status"));
                    }
                };
                self.last_known_status = new_status;
                self.send_response::<()>(request, Ok(None))?;
                let event_body = Some(StoppedEventBody {
                    reason: "step".to_owned(),
                    description: Some(format!(
                        "{} at address {:#010x}",
                        new_status.short_long_status().1,
                        cpu_info.pc
                    )),
                    thread_id: Some(core_data.target_core.id() as i64),
                    preserve_focus_hint: None,
                    text: None,
                    all_threads_stopped: Some(false), // TODO: Implement multi-core logic here
                    hit_breakpoint_ids: None,
                });
                self.send_event("stopped", event_body)
            }
            Err(error) => {
                self.send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error))))
            }
        }
    }

    /// The DAP protocol uses three related values to determine how to invoke the `Variables` request.
    /// This function retrieves that information from the `DebugInfo::VariableCache` and returns it as
    /// (`variable_reference`, `named_child_variables_cnt`, `indexed_child_variables_cnt`)
    fn get_variable_reference(
        &mut self,
        parent_variable: &probe_rs::debug::Variable,
        cache: &mut VariableCache,
    ) -> (i64, i64, i64) {
        if !parent_variable.is_valid() {
            return (0, 0, 0);
        }
        let mut named_child_variables_cnt = 0;
        let mut indexed_child_variables_cnt = 0;
        if let Ok(children) = cache.get_children(Some(parent_variable.variable_key)) {
            for child_variable in children {
                if child_variable.is_indexed() {
                    indexed_child_variables_cnt += 1;
                } else {
                    named_child_variables_cnt += 1;
                }
            }
        };

        if named_child_variables_cnt > 0 || indexed_child_variables_cnt > 0 {
            (
                parent_variable.variable_key,
                named_child_variables_cnt,
                indexed_child_variables_cnt,
            )
        } else if parent_variable.variable_node_type.is_deferred()
            && parent_variable.get_value(cache) != "()"
        {
            // TODO: We should implement changing unit types to VariableNodeType::DoNotRecurse
            // We have not yet cached the children for this reference.
            // Provide DAP Client with a reference so that it will explicitly ask for children when the user expands it.
            (parent_variable.variable_key, 0, 0)
        } else {
            // Returning 0's allows VSCode DAP Client to behave correctly for frames that have no variables, and variables that have no children.
            (0, 0, 0)
        }
    }

    /// Returns one of the standard DAP Requests if all goes well, or a "error" request, which should indicate that the calling function should return.
    /// When preparing to return an "error" request, we will send a Response containing the DebuggerError encountered.
    pub fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>> {
        self.adapter.listen_for_request()
    }

    /// Sends either the success response or an error response if passed a
    /// DebuggerError. For the DAP Client, it forwards the response, while for
    /// the CLI, it will print the body for success, or the message for
    /// failure.
    pub fn send_response<S: Serialize>(
        &mut self,
        request: Request,
        response: Result<Option<S>, DebuggerError>,
    ) -> Result<()> {
        self.adapter.send_response(request, response)
    }

    pub fn send_error_response(&mut self, response: &DebuggerError) -> Result<()> {
        if self
            .adapter
            .show_message(MessageSeverity::Error, response.to_string())
        {
            Ok(())
        } else {
            Err(anyhow!("Failed to send error response"))
        }
    }

    pub fn send_event<S: Serialize>(
        &mut self,
        event_type: &str,
        event_body: Option<S>,
    ) -> Result<()> {
        self.adapter.send_event(event_type, event_body)
    }

    pub fn log_to_console<S: Into<String>>(&mut self, message: S) -> bool {
        self.adapter.log_to_console(message)
    }

    /// Send a custom "probe-rs-show-message" event to the MS DAP Client.
    /// The `severity` field can be one of `information`, `warning`, or `error`.
    pub fn show_message(&mut self, severity: MessageSeverity, message: impl Into<String>) -> bool {
        self.adapter.show_message(severity, message)
    }

    /// Send a custom `probe-rs-rtt-channel-config` event to the MS DAP Client, to create a window for a specific RTT channel.
    pub fn rtt_window(
        &mut self,
        channel_number: usize,
        channel_name: String,
        data_format: rtt::DataFormat,
    ) -> bool {
        let event_body = match serde_json::to_value(RttChannelEventBody {
            channel_number,
            channel_name,
            data_format,
        }) {
            Ok(event_body) => event_body,
            Err(_) => {
                return false;
            }
        };
        self.send_event("probe-rs-rtt-channel-config", Some(event_body))
            .is_ok()
    }

    /// Send a custom `probe-rs-rtt-data` event to the MS DAP Client, to
    pub fn rtt_output(&mut self, channel_number: usize, rtt_data: String) -> bool {
        let event_body = match serde_json::to_value(RttDataEventBody {
            channel_number,
            data: rtt_data,
        }) {
            Ok(event_body) => event_body,
            Err(_) => {
                return false;
            }
        };
        self.send_event("probe-rs-rtt-data", Some(event_body))
            .is_ok()
    }

    fn new_progress_id(&mut self) -> ProgressId {
        let id = self.progress_id;

        self.progress_id += 1;

        id
    }

    pub fn start_progress(&mut self, title: &str, request_id: Option<i64>) -> Result<ProgressId> {
        anyhow::ensure!(
            self.supports_progress_reporting,
            "Progress reporting is not supported by client."
        );

        let progress_id = self.new_progress_id();

        self.send_event(
            "progressStart",
            Some(ProgressStartEventBody {
                cancellable: Some(false),
                message: None,
                percentage: None,
                progress_id: progress_id.to_string(),
                request_id,
                title: title.to_owned(),
            }),
        )?;

        Ok(progress_id)
    }

    pub fn end_progress(&mut self, progress_id: ProgressId) -> Result<()> {
        anyhow::ensure!(
            self.supports_progress_reporting,
            "Progress reporting is not supported by client."
        );

        self.send_event(
            "progressEnd",
            Some(ProgressEndEventBody {
                message: None,
                progress_id: progress_id.to_string(),
            }),
        )
    }

    /// Update the progress report in VSCode.
    /// The progress has the range [0..1].
    pub fn update_progress(
        &mut self,
        progress: f64,
        message: Option<impl Into<String>>,
        progress_id: i64,
    ) -> Result<ProgressId> {
        anyhow::ensure!(
            self.supports_progress_reporting,
            "Progress reporting is not supported by client."
        );

        let _ok = self.send_event(
            "progressUpdate",
            Some(ProgressUpdateEventBody {
                message: message.map(|v| v.into()),
                percentage: Some(progress * 100.0),
                progress_id: progress_id.to_string(),
            }),
        )?;

        Ok(progress_id)
    }

    pub(crate) fn set_console_log_level(&mut self, error: ConsoleLog) {
        self.adapter.set_console_log_level(error)
    }
}

/// Provides halt functionality that is re-used elsewhere, in context of multiple DAP Requests
pub(crate) fn halt_core(
    target_core: &mut probe_rs::Core,
) -> Result<probe_rs::CoreInformation, DebuggerError> {
    match target_core.halt(Duration::from_millis(100)) {
        Ok(cpu_info) => Ok(cpu_info),
        Err(error) => Err(DebuggerError::Other(anyhow!("{}", error))),
    }
}

pub fn get_arguments<T: DeserializeOwned>(req: &Request) -> Result<T, crate::DebuggerError> {
    let value = req
        .arguments
        .as_ref()
        .ok_or(crate::DebuggerError::InvalidRequest)?;

    serde_json::from_value(value.to_owned()).map_err(|e| e.into())
}

pub(crate) trait DapStatus {
    fn short_long_status(&self) -> (&'static str, &'static str);
}
impl DapStatus for CoreStatus {
    /// Return a tuple with short and long descriptions of the core status for human machine interface / hmi. The short status matches with the strings implemented by the Microsoft DAP protocol, e.g. `let (short_status, long status) = CoreStatus::short_long_status(core_status)`
    fn short_long_status(&self) -> (&'static str, &'static str) {
        match self {
            CoreStatus::Running => ("continued", "Core is running"),
            CoreStatus::Sleeping => ("sleeping", "Core is in SLEEP mode"),
            CoreStatus::LockedUp => (
                "lockedup",
                "Core is in LOCKUP status - encountered an unrecoverable exception",
            ),
            CoreStatus::Halted(halt_reason) => match halt_reason {
                HaltReason::Breakpoint => (
                    "breakpoint",
                    "Core halted due to a breakpoint (software or hardware)",
                ),
                HaltReason::Exception => (
                    "exception",
                    "Core halted due to an exception, e.g. interupt handler",
                ),
                HaltReason::Watchpoint => (
                    "data breakpoint",
                    "Core halted due to a watchpoint or data breakpoint",
                ),
                HaltReason::Step => ("step", "Core halted after a 'step' instruction"),
                HaltReason::Request => (
                    "pause",
                    "Core halted due to a user (debugger client) request",
                ),
                HaltReason::External => ("external", "Core halted due to an external request"),
                _other => ("unrecognized", "Core halted: unrecognized cause"),
            },
            CoreStatus::Unknown => ("unknown", "Core status cannot be determined"),
        }
    }
}
