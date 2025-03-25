use super::{
    core_status::DapStatus,
    dap_types,
    repl_commands_helpers::{build_expanded_commands, command_completions},
    request_helpers::{
        disassemble_target_memory, get_dap_source, get_svd_variable_reference,
        get_variable_reference, set_instruction_breakpoint,
    },
};
use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::protocol::{ProtocolAdapter, ProtocolHelper},
    server::{
        configuration::ConsoleLog,
        core_data::CoreHandle,
        session_data::{BreakpointType, SourceLocationScope},
    },
};
use crate::util::rtt;
use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose as base64_engine};
use dap_types::*;
use parse_int::parse;
use probe_rs::{
    Architecture::Riscv,
    CoreStatus, Error, HaltReason, MemoryInterface, RegisterValue,
    architecture::{
        arm::ArmError, riscv::communication_interface::RiscvError,
        xtensa::communication_interface::XtensaError,
    },
};
use probe_rs_debug::{
    ColumnType, ObjectRef, SourceLocation, SteppingMode, VariableName, VerifiedBreakpoint,
    stack_frame::StackFrameInfo,
};
use serde::{Serialize, de::DeserializeOwned};
use typed_path::NativePathBuf;

use std::{fmt::Display, str, time::Duration};

/// Progress ID used for progress reporting when the debug adapter protocol is used.
type ProgressId = i64;

/// A Debug Adapter Protocol "Debug Adapter",
/// see <https://microsoft.github.io/debug-adapter-protocol/overview>
pub struct DebugAdapter<P: ProtocolAdapter> {
    pub(crate) halt_after_reset: bool,
    /// NOTE: VSCode sends a 'threads' request when it receives the response from the `ConfigurationDone` request, irrespective of target state.
    /// This can lead to duplicate `threads->stacktrace->etc.` sequences if & when the target halts and sends a 'stopped' event.
    /// See <https://github.com/golang/vscode-go/issues/940> for more info.
    /// In order to avoid overhead and duplicate responses, we will implement the following logic.
    /// - `configuration_done` will ignore target status, and simply notify VSCode when it is done.
    /// - `threads` will check for [DebugAdapter::configuration_done] and ...
    ///   - If it is `false`, it will ...
    ///     - send back a threads response, with `all_threads_stopped=Some(false)`, and set [DebugAdapter::configuration_done] to `true`.
    ///   - If it is `true`, it will respond with thread information as expected.
    configuration_done: bool,
    /// Flag to indicate if all cores of the target are halted. This is used to accurately report the `all_threads_stopped` field in the DAP `StoppedEvent`,
    /// as well as to prevent unnecessary polling of core status.
    /// The default is `true`, and will be set to `false` if any of the cores report a status other than `CoreStatus::Halted(_)`.
    pub(crate) all_cores_halted: bool,
    /// Progress ID used for progress reporting when the debug adapter protocol is used.
    progress_id: ProgressId,
    /// Flag to indicate if the connected client supports progress reporting.
    pub(crate) supports_progress_reporting: bool,
    /// Flags to improve breakpoint accuracy.
    /// DWARF spec at Sect 2.14 uses 1 based numbering, with a 0 indicating not-specified. We will follow that standard,
    /// and translate incoming requests depending on the DAP Client treatment of 0 or 1 based numbering.
    pub(crate) lines_start_at_1: bool,
    /// DWARF spec at Sect 2.14 uses 1 based numbering, with a 0 indicating not-specified. We will follow that standard,
    /// and translate incoming requests depending on the DAP Client treatment of 0 or 1 based numbering.
    pub(crate) columns_start_at_1: bool,
    /// Flag to indicate that workarounds for VSCode-specific spec deviations etc. should be
    /// enabled.
    pub(crate) vscode_quirks: bool,
    adapter: P,
}

impl<P: ProtocolAdapter> DebugAdapter<P> {
    pub fn new(adapter: P) -> DebugAdapter<P> {
        DebugAdapter {
            vscode_quirks: false,
            halt_after_reset: false,
            configuration_done: false,
            all_cores_halted: true,
            progress_id: 0,
            supports_progress_reporting: false,
            lines_start_at_1: true,
            columns_start_at_1: true,
            adapter,
        }
    }

    pub(crate) fn configuration_is_done(&self) -> bool {
        self.configuration_done
    }

    pub(crate) fn pause(&mut self, target_core: &mut CoreHandle, request: &Request) -> Result<()> {
        match target_core.core.halt(Duration::from_millis(500)) {
            Ok(cpu_info) => {
                let new_status = match target_core.core.status() {
                    Ok(new_status) => new_status,
                    Err(error) => {
                        self.send_response::<()>(request, Err(&DebuggerError::ProbeRs(error)))?;
                        return Err(anyhow!("Failed to retrieve core status"));
                    }
                };
                self.send_response(
                    request,
                    Ok(Some(format!(
                        "Core stopped at address {:#010x}",
                        cpu_info.pc
                    ))),
                )?;
                let event_body = Some(StoppedEventBody {
                    reason: "pause".to_owned(),
                    description: Some(new_status.short_long_status(Some(cpu_info.pc)).1),
                    thread_id: Some(target_core.core.id() as i64),
                    preserve_focus_hint: Some(false),
                    text: None,
                    all_threads_stopped: Some(self.all_cores_halted),
                    hit_breakpoint_ids: None,
                });
                // We override the halt reason to prevent duplicate stopped events.
                target_core.core_data.last_known_status = CoreStatus::Halted(HaltReason::Request);

                self.send_event("stopped", event_body)?;
                Ok(())
            }
            Err(error) => {
                self.send_response::<()>(request, Err(&DebuggerError::Other(anyhow!("{}", error))))
            }
        }
    }

    pub(crate) fn disconnect(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        let arguments: DisconnectArguments = get_arguments(self, request)?;

        // TODO: For now (until we do multicore), we will assume that both terminate and suspend translate to a halt of the core.
        let must_halt_debuggee = arguments.terminate_debuggee.unwrap_or(false)
            || arguments.suspend_debuggee.unwrap_or(false);

        if must_halt_debuggee {
            let _ = target_core.core.halt(Duration::from_millis(100));
        }

        self.send_response::<DisconnectResponse>(request, Ok(None))
    }

    pub(crate) fn read_memory(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        let arguments: ReadMemoryArguments = get_arguments(self, request)?;

        let memory_offset = arguments.offset.unwrap_or(0);
        let mut address: u64 = match parse::<u64>(arguments.memory_reference.as_ref()) {
            Ok(address) => address + memory_offset as u64,
            Err(err) => {
                return self.send_response::<()>(
                    request,
                    Err(&DebuggerError::Other(anyhow!(
                        "Failed to parse memory reference {:?}: {err}",
                        arguments.memory_reference
                    ))),
                );
            }
        };
        let mut num_bytes_unread = arguments.count as usize;
        // The probe-rs API does not return partially read data.
        // It either succeeds for the whole buffer or not. However, doing single byte reads is slow, so we will
        // do reads in larger chunks, until we get an error, and then do single byte reads for the last few bytes, to make
        // sure we get all the data we can.
        let mut result_buffer = vec![];
        let large_read_byte_count = 8usize;
        let mut fast_buff = vec![0u8; large_read_byte_count];
        // Read as many large chunks as possible.
        while num_bytes_unread > 0 {
            if let Ok(()) = target_core.core.read(address, &mut fast_buff) {
                result_buffer.extend_from_slice(&fast_buff);
                address += large_read_byte_count as u64;
                num_bytes_unread -= large_read_byte_count;
            } else {
                break;
            }
        }
        // Read the remaining bytes one by one.
        while num_bytes_unread > 0 {
            if let Ok(good_byte) = target_core.core.read_word_8(address) {
                result_buffer.push(good_byte);
                address += 1;
                num_bytes_unread -= 1;
            } else {
                break;
            }
        }
        // Currently, VSCode sends a request with count=0 after the last successful one ... so
        // let's ignore it.
        if !result_buffer.is_empty() || (self.vscode_quirks && arguments.count == 0) {
            let response = base64_engine::STANDARD.encode(&result_buffer);
            self.send_response(
                request,
                Ok(Some(ReadMemoryResponseBody {
                    address: format!("{address:#010x}"),
                    data: Some(response),
                    unreadable_bytes: if num_bytes_unread == 0 {
                        None
                    } else {
                        Some(num_bytes_unread as i64)
                    },
                })),
            )
        } else {
            self.send_response::<()>(
                request,
                Err(&DebuggerError::Other(anyhow!(
                    "Could not read any data at address {:#010x}",
                    address
                ))),
            )
        }
    }

    pub(crate) fn write_memory(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        let arguments: WriteMemoryArguments = get_arguments(self, request)?;
        let memory_offset = arguments.offset.unwrap_or(0);
        let address: u64 = if let Ok(address) = parse::<i64>(arguments.memory_reference.as_ref()) {
            match (address + memory_offset).try_into() {
                    Ok(modified_address) => modified_address,
                    Err(error) => return self.send_response::<()>(
                    request,
                    Err(&DebuggerError::Other(anyhow!(
                        "Could not convert memory_reference: {} and offset: {:?} into a 32-bit memory address: {:?}",
                        arguments.memory_reference, arguments.offset, error
                    ))),
                ),
                }
        } else {
            return self.send_response::<()>(
                request,
                Err(&DebuggerError::Other(anyhow!(
                    "Could not read any data at address {:?}",
                    arguments.memory_reference
                ))),
            );
        };
        let data_bytes = match base64_engine::STANDARD.decode(&arguments.data) {
            Ok(decoded_bytes) => decoded_bytes,
            Err(error) => {
                return self.send_response::<()>(
                    request,
                    Err(&DebuggerError::Other(anyhow!(
                        "Could not decode base64 data:{:?} :  {:?}",
                        arguments.data,
                        error
                    ))),
                );
            }
        };
        match target_core
            .core
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
                // TODO: This doesn't trigger the VSCode UI to reload the variables effected.
                // Investigate if we can force it in some other way, or if it is a known issue.
                self.send_event(
                    "memory",
                    Some(MemoryEventBody {
                        count: data_bytes.len() as i64,
                        memory_reference: format!("{address:#010x}"),
                        offset: 0,
                    }),
                )
            }
            Err(error) => self.send_response::<()>(request, Err(&error)),
        }
    }

    /// Evaluates the given expression in the context of the top most stack frame.
    /// The expression has access to any variables and arguments that are in scope.
    pub(crate) fn evaluate(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        // TODO: When variables appear in the `watch` context, they will not resolve correctly after a 'step' function. Consider doing the lazy load for 'either/or' of Variables vs. Evaluate

        let arguments: EvaluateArguments = get_arguments(self, request)?;

        // Various fields in the response_body will be updated before we return.
        let mut response_body = EvaluateResponseBody {
            indexed_variables: None,
            memory_reference: None,
            named_variables: None,
            presentation_hint: None,
            result: format!("<invalid expression {:?}>", arguments.expression),
            type_: None,
            variables_reference: 0_i64,
        };

        if let Some(context) = &arguments.context {
            if context == "clipboard" {
                response_body.result = arguments.expression;
            } else if context == "repl" {
                match self.handle_repl(target_core, &arguments) {
                    Ok(repl_response) => {
                        // In all other cases, the response would have been updated by the repl command handler.
                        response_body.result = if repl_response.success {
                            repl_response
                                .message
                                // This should always have a value, but just in case someone was lazy ...
                                .unwrap_or_else(|| "Success.".to_string())
                        } else {
                            format!(
                                "Error: {:?} {:?}",
                                repl_response.command, repl_response.message
                            )
                        };

                        // Perform any special post-processing of the response.
                        match repl_response.command.as_str() {
                            "terminate" => {
                                // This is a special case, where a repl command has requested that the debug session be terminated.
                                self.send_event(
                                    "terminated",
                                    Some(TerminatedEventBody { restart: None }),
                                )?;
                            }
                            "variables" => {
                                // This is a special case, where a repl command has requested that the variables be displayed.
                                if let Some(repl_response_body) = repl_response.body {
                                    if let Ok(evaluate_response) =
                                        serde_json::from_value(repl_response_body.clone())
                                    {
                                        response_body = evaluate_response;
                                    } else {
                                        response_body.result = format!(
                                            "Error: Could not parse response body: {repl_response_body:?}"
                                        );
                                    }
                                }
                            }
                            "setBreakpoints" => {
                                // This is a special case, where we've added a breakpoint, and need to synch the DAP client UI.
                                self.send_event("breakpoint", repl_response.body)?;
                            }
                            _other_commands => {}
                        }
                    }
                    Err(error) => {
                        response_body.result = match error {
                            DebuggerError::UserMessage(repl_message) => repl_message,
                            other_error => format!("{other_error:?}"),
                        };
                    }
                }
            } else {
                // Handle other contexts: 'watch', 'hover', etc.
                // The Variables request sometimes returns the variable name, and other times the variable id, so this expression will be tested to determine if it is an id or not.
                let expression = arguments.expression.clone();

                // Make sure we have a valid StackFrame
                if let Some(stack_frame) =
                    match arguments.frame_id.map(ObjectRef::try_from).transpose() {
                        Ok(Some(frame_id)) => target_core
                            .core_data
                            .stack_frames
                            .iter_mut()
                            .find(|stack_frame| stack_frame.id == frame_id),
                        Ok(None) => {
                            // Use the current frame_id
                            target_core.core_data.stack_frames.first_mut()
                        }
                        Err(e) => {
                            tracing::warn!("Invalid frame_id: {e}");
                            // Use the current frame_id
                            target_core.core_data.stack_frames.first_mut()
                        }
                    }
                {
                    // Always search the registers first, because we don't have a VariableCache for them.
                    if let Some(register_value) = stack_frame
                        .registers
                        .get_register_by_name(expression.as_str())
                        .and_then(|reg| reg.value)
                    {
                        response_body.type_ = Some(format!("{}", VariableName::RegistersRoot));
                        response_body.result = format!("{register_value}");
                    } else {
                        // If the expression wasn't pointing to a register, then check if is a local or static variable in our stack_frame
                        let mut variable: Option<probe_rs_debug::Variable> = None;
                        let mut variable_cache: Option<&mut probe_rs_debug::VariableCache> = None;
                        // Search through available caches and stop as soon as the variable is found
                        if let Some(search_cache) = stack_frame.local_variables.as_mut() {
                            if search_cache.len() == 1 {
                                let mut root_variable = search_cache.root_variable().clone();

                                // This is a special case where we have a single variable in the cache, and it is the root of a scope.
                                // These variables don't have cached children by default, so we need to resolve them before we proceed.
                                // We check for len() == 1, so unwrap() on first_mut() is safe.
                                target_core.core_data.debug_info.cache_deferred_variables(
                                    search_cache,
                                    &mut target_core.core,
                                    &mut root_variable,
                                    StackFrameInfo {
                                        registers: &stack_frame.registers,
                                        frame_base: stack_frame.frame_base,
                                        canonical_frame_address: stack_frame
                                            .canonical_frame_address,
                                    },
                                )?;
                            }

                            if let Ok(expression_as_key) = expression.parse::<ObjectRef>() {
                                variable = search_cache.get_variable_by_key(expression_as_key);
                            } else {
                                variable = search_cache
                                    .get_variable_by_name(&VariableName::Named(expression.clone()));
                            }
                            if variable.is_some() {
                                variable_cache = Some(search_cache);
                            }
                        }
                        // Check if we found a variable.
                        if let (Some(variable), Some(variable_cache)) = (variable, variable_cache) {
                            let (
                                variables_reference,
                                named_child_variables_cnt,
                                indexed_child_variables_cnt,
                            ) = get_variable_reference(&variable, variable_cache);
                            response_body.indexed_variables = Some(indexed_child_variables_cnt);
                            response_body.memory_reference =
                                Some(variable.memory_location.to_string());
                            response_body.named_variables = Some(named_child_variables_cnt);
                            response_body.result = variable.to_string(variable_cache);
                            response_body.type_ = Some(variable.type_name());
                            response_body.variables_reference = variables_reference.into();
                        } else {
                            // If we made it to here, no register or variable matched the expression.
                            for variable_cache_entry in [target_core
                                .core_data
                                .core_peripherals
                                .as_ref()
                                .map(|core_peripherals| &core_peripherals.svd_variable_cache)]
                            .into_iter()
                            .flatten()
                            {
                                let svd_variable = if let Ok(expression_as_key) =
                                    expression.parse::<ObjectRef>()
                                {
                                    variable_cache_entry.get_variable_by_key(expression_as_key)
                                } else {
                                    variable_cache_entry.get_variable_by_name(&expression)
                                };

                                if let Some(svd_variable) = svd_variable {
                                    let (variables_reference, named_child_variables_cnt) =
                                        get_svd_variable_reference(
                                            svd_variable,
                                            variable_cache_entry,
                                        );
                                    response_body.indexed_variables = None;
                                    response_body.memory_reference =
                                        svd_variable.memory_reference();
                                    response_body.named_variables = Some(named_child_variables_cnt);
                                    response_body.result =
                                        svd_variable.get_value(&mut target_core.core);
                                    response_body.type_ = svd_variable.type_name();
                                    response_body.variables_reference = variables_reference.into();
                                }
                            }
                        }
                    }
                }
            }
        }
        self.send_response(request, Ok(Some(response_body)))
    }

    fn handle_repl(
        &mut self,
        target_core: &mut CoreHandle<'_>,
        arguments: &EvaluateArguments,
    ) -> Result<Response, DebuggerError> {
        if !target_core.core.core_halted()?
            && !arguments.expression.starts_with("break")
            && !arguments.expression.starts_with("quit")
            && !arguments.expression.starts_with("help")
        {
            return Err(DebuggerError::UserMessage(
                "The target is running. Only the 'break', 'help' or 'quit' commands are allowed."
                    .to_string(),
            ));
        }

        // The target is halted, so we can allow any repl command.
        //TODO: Do we need to look for '/' in the expression, before we split it?
        // Now we can make sure we have a valid expression and evaluate it.
        let (command_root, repl_commands) = build_expanded_commands(arguments.expression.trim());

        let Some(repl_command) = repl_commands.first() else {
            return Err(DebuggerError::UserMessage(format!(
                "Invalid REPL command: {:?}.",
                command_root
            )));
        };

        // We have a valid repl command, so we can evaluate it.
        // First, let's extract the remainder of the arguments, so that we can pass them to the handler.
        let argument_string = arguments
            .expression
            .trim_start_matches(&command_root)
            .trim_start()
            .trim_start_matches(repl_command.command)
            .trim_start();

        (repl_command.handler)(target_core, argument_string, arguments)
    }

    /// Works in tandem with the `evaluate` request, to provide possible completions in the Debug Console REPL window.
    pub(crate) fn completions(&mut self, _: &mut CoreHandle, request: &Request) -> Result<()> {
        // TODO: When variables appear in the `watch` context, they will not resolve correctly after a 'step' function. Consider doing the lazy load for 'either/or' of Variables vs. Evaluate

        let arguments: CompletionsArguments = get_arguments(self, request)?;

        let response_body = CompletionsResponseBody {
            targets: command_completions(arguments),
        };

        self.send_response(request, Ok(Some(response_body)))
    }

    /// Set the variable with the given name in the variable container to a new value.
    pub(crate) fn set_variable(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        let arguments: SetVariableArguments = get_arguments(self, request)?;

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
        let parent_key: ObjectRef = arguments.variables_reference.into();
        let new_value = &arguments.value;

        //TODO: Check for, and prevent SVD Peripheral/Register/Field values from being updated, until such time as we can do it safely.

        match target_core
            .core_data
            .stack_frames
            .iter_mut()
            .find(|stack_frame| stack_frame.id == parent_key)
        {
            Some(stack_frame) => {
                // The variable is a register value in this StackFrame
                if let Some(_register_value) = stack_frame
                    .registers
                    .get_register_by_name(arguments.name.as_str())
                    .and_then(|reg| reg.value)
                {
                    // TODO: Does it make sense for us to consider implementing an update of platform registers?
                    return self.send_response::<SetVariableResponseBody>(
                        request,
                        Err(&DebuggerError::Other(anyhow!(
                            "Set Register values is not yet supported."
                        ))),
                    );
                }
            }
            None => {
                let variable_name = VariableName::Named(arguments.name.clone());

                // The parent_key refers to a local or static variable in one of the in-scope StackFrames.
                let mut cache_variable: Option<probe_rs_debug::Variable> = None;
                let mut variable_cache: Option<&mut probe_rs_debug::VariableCache> = None;
                for search_frame in target_core.core_data.stack_frames.iter_mut() {
                    if let Some(search_cache) = &mut search_frame.local_variables {
                        if let Some(search_variable) =
                            search_cache.get_variable_by_name_and_parent(&variable_name, parent_key)
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
                        &mut target_core.core,
                        variable_cache,
                        new_value.clone(),
                    ) {
                        Ok(()) => {
                            let (
                                variables_reference,
                                named_child_variables_cnt,
                                indexed_child_variables_cnt,
                            ) = get_variable_reference(&cache_variable, variable_cache);
                            response_body.variables_reference = Some(variables_reference.into());
                            response_body.named_variables = Some(named_child_variables_cnt);
                            response_body.indexed_variables = Some(indexed_child_variables_cnt);
                            response_body.type_ = Some(format!("{:?}", cache_variable.type_name));
                            response_body.value.clone_from(new_value);
                        }
                        Err(error) => {
                            return self.send_response::<SetVariableResponseBody>(
                                request,
                                Err(&DebuggerError::Other(anyhow!(
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
                                Err(&DebuggerError::Other(anyhow!(
                                    "Failed to update variable: {}, with new value {:?} : Please report this as a bug.",
                                    arguments.name,
                                    arguments.value
                                ))),
                            )
        } else {
            self.send_response(request, Ok(Some(response_body)))
        }
    }

    pub(crate) fn restart(
        &mut self,
        target_core: &mut CoreHandle,
        request: Option<&Request>,
    ) -> Result<()> {
        match target_core.core.halt(Duration::from_millis(500)) {
            Ok(_) => {}
            Err(error) => {
                if let Some(request) = request {
                    return self.send_response::<()>(
                        request,
                        Err(&DebuggerError::Other(anyhow!("{}", error))),
                    );
                } else {
                    return self.show_error_message(&DebuggerError::Other(anyhow!("{}", error)));
                }
            }
        }

        target_core.reset_core_status(self);

        // Different code paths if we invoke this from a request, versus an internal function.
        if let Some(request) = request {
            // Use reset_and_halt(), and then resume again afterwards, depending on the reset_after_halt flag.
            if let Err(error) = target_core.core.reset_and_halt(Duration::from_millis(500)) {
                return self.send_response::<()>(
                    request,
                    Err(&DebuggerError::Other(anyhow!("{}", error))),
                );
            }

            // Ensure ebreak enters debug mode, this is necessary for soft breakpoints to work on architectures like RISC-V.
            target_core.core.debug_on_sw_breakpoint(true)?;

            // For RISC-V, we need to re-enable any breakpoints that were previously set, because the core reset 'forgets' them.
            if target_core.core.architecture() == Riscv {
                let saved_breakpoints = std::mem::take(&mut target_core.core_data.breakpoints);

                for breakpoint in saved_breakpoints {
                    match target_core
                        .set_breakpoint(breakpoint.address, breakpoint.breakpoint_type.clone())
                    {
                        Ok(_) => {}
                        Err(error) => {
                            //This will cause the debugger to show the user an error, but not stop the debugger.
                            tracing::error!(
                                "Failed to re-enable breakpoint {:?} after reset. {}",
                                breakpoint,
                                error
                            );
                        }
                    }
                }
            }

            // Now that we have the breakpoints re-enabled, we can decide if it is appropriate to resume the core.
            if !self.halt_after_reset {
                match self.r#continue(target_core, request) {
                    Ok(_) => {
                        self.send_response::<()>(request, Ok(None))?;
                        let event_body = Some(ContinuedEventBody {
                            all_threads_continued: Some(false), // TODO: Implement multi-core logic here
                            thread_id: target_core.core.id() as i64,
                        });
                        self.send_event("continued", event_body)?;
                        Ok(())
                    }
                    Err(error) => self.send_response::<()>(
                        request,
                        Err(&DebuggerError::Other(anyhow!("{}", error))),
                    ),
                }
            } else {
                self.send_response::<()>(request, Ok(None))?;
                let event_body = Some(StoppedEventBody {
                    reason: "restart".to_owned(),
                    description: Some(
                        CoreStatus::Halted(HaltReason::External)
                            .short_long_status(None)
                            .1,
                    ),
                    thread_id: Some(target_core.core.id() as i64),
                    preserve_focus_hint: None,
                    text: None,
                    all_threads_stopped: Some(self.all_cores_halted),
                    hit_breakpoint_ids: None,
                });
                self.send_event("stopped", event_body)?;
                Ok(())
            }
        } else {
            // The DAP Client will always do a `reset_and_halt`, and then will consider `halt_after_reset` value after the `configuration_done` request.
            // Otherwise the probe will run past the `main()` before the DAP Client has had a chance to set breakpoints in `main()`.
            let core_info = match target_core.core.reset_and_halt(Duration::from_millis(500)) {
                Ok(core_info) => core_info,
                Err(error) => {
                    return self.show_error_message(&DebuggerError::Other(anyhow!("{}", error)));
                }
            };

            // Ensure ebreak enters debug mode, this is necessary for soft breakpoints to work on architectures like RISC-V.
            target_core.core.debug_on_sw_breakpoint(true)?;

            // Only notify the DAP client if we are NOT in initialization stage ([`DebugAdapter::configuration_done`]).
            if self.configuration_is_done() {
                let event_body = Some(StoppedEventBody {
                    reason: "restart".to_owned(),
                    description: Some(
                        CoreStatus::Halted(HaltReason::External)
                            .short_long_status(Some(core_info.pc))
                            .1,
                    ),
                    thread_id: Some(target_core.core.id() as i64),
                    preserve_focus_hint: None,
                    text: None,
                    all_threads_stopped: Some(self.all_cores_halted),
                    hit_breakpoint_ids: None,
                });
                self.send_event("stopped", event_body)?;
            }
            Ok(())
        }
    }

    #[tracing::instrument(level = "debug", skip_all, name = "Handle configuration done")]
    pub(crate) fn configuration_done(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        let current_core_status = target_core.core.status()?;

        if current_core_status.is_halted() {
            if self.halt_after_reset
                || matches!(
                    current_core_status,
                    CoreStatus::Halted(HaltReason::Breakpoint(_))
                )
            {
                let program_counter = target_core
                    .core
                    .read_core_reg(target_core.core.program_counter())
                    .ok();
                let event_body = Some(StoppedEventBody {
                    reason: current_core_status
                        .short_long_status(program_counter)
                        .0
                        .to_owned(),
                    description: Some(current_core_status.short_long_status(program_counter).1),
                    thread_id: Some(target_core.core.id() as i64),
                    preserve_focus_hint: None,
                    text: None,
                    all_threads_stopped: Some(self.all_cores_halted),
                    hit_breakpoint_ids: None,
                });
                self.send_event("stopped", event_body)?;
            } else {
                tracing::debug!(
                    "Core is halted, but not due to a breakpoint and halt_after_reset is not set. Continuing."
                );
                self.r#continue(target_core, request)?;
            }
        }

        self.configuration_done = true;
        self.send_response::<()>(request, Ok(None))
    }

    pub(crate) fn set_breakpoints(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        let args: SetBreakpointsArguments = get_arguments(self, request)?;

        let mut created_breakpoints: Vec<Breakpoint> = Vec::new(); // For returning in the Response

        if let Some(source_path) = args.source.path.as_ref() {
            // Always clear existing breakpoints for the specified `[crate::debug_adapter::dap_types::Source]` before setting new ones.
            // The DAP Specification doesn't make allowances for deleting and setting individual breakpoints for a specific `Source`.
            match target_core.clear_breakpoints(BreakpointType::SourceBreakpoint {
                source: Box::new(args.source.clone()),
                location: SourceLocationScope::All,
            }) {
                Ok(_) => {}
                Err(error) => {
                    return self.send_response::<()>(
                        request,
                        Err(&DebuggerError::Other(anyhow!(
                            "Failed to clear existing breakpoints before setting new ones : {}",
                            error
                        ))),
                    );
                }
            }

            // Assume that the path is native to the current OS
            let source_path = NativePathBuf::from(source_path).to_typed_path_buf();

            if let Some(requested_breakpoints) = args.breakpoints.as_ref() {
                for bp in requested_breakpoints {
                    // Some overrides to improve breakpoint accuracy when `DebugInfo::get_breakpoint_location()` has to select the best from multiple options
                    let requested_breakpoint_line = if self.lines_start_at_1 {
                        // If the debug client uses 1 based numbering, then we can use it as is.
                        bp.line as u64
                    } else {
                        // If the debug client uses 0 based numbering, then we bump the number by 1
                        bp.line as u64 + 1
                    };
                    let requested_breakpoint_column = if self.columns_start_at_1 {
                        // If the debug client uses 1 based numbering, then we can use it as is.
                        Some(bp.column.unwrap_or(1) as u64)
                    } else {
                        // If the debug client uses 0 based numbering, then we bump the number by 1
                        Some(bp.column.unwrap_or(0) as u64 + 1)
                    };

                    match target_core.verify_and_set_breakpoint(
                        source_path.to_path(),
                        requested_breakpoint_line,
                        requested_breakpoint_column,
                        &args.source,
                    ) {
                        Ok(VerifiedBreakpoint {
                            address,
                            source_location,
                        }) => created_breakpoints.push(Breakpoint {
                            column: source_location.column.map(|col| match col {
                                ColumnType::LeftEdge => 0_i64,
                                ColumnType::Column(c) => c as i64,
                            }),
                            end_column: None,
                            end_line: None,
                            id: None,
                            line: source_location.line.map(|line| line as i64),
                            message: Some(format!(
                                "Source breakpoint at memory address: {address:#010X}"
                            )),
                            source: Some(args.source.clone()),
                            instruction_reference: Some(format!("{address:#010X}")),
                            offset: None,
                            verified: true,
                        }),
                        Err(error) => created_breakpoints.push(Breakpoint {
                            column: None,
                            end_column: None,
                            end_line: None,
                            id: None,
                            line: Some(bp.line),
                            message: Some(error.to_string()),
                            source: None,
                            instruction_reference: None,
                            offset: None,
                            verified: false,
                        }),
                    };
                }
            }

            let breakpoint_body = SetBreakpointsResponseBody {
                breakpoints: created_breakpoints,
            };
            self.send_response(request, Ok(Some(breakpoint_body)))
        } else {
            self.send_response::<()>(
                request,
                Err(&DebuggerError::Other(anyhow!(
                    "Could not get a valid source path from arguments: {args:?}"
                ))),
            )
        }
    }

    pub(crate) fn set_instruction_breakpoints(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        let arguments: SetInstructionBreakpointsArguments = get_arguments(self, request)?;

        // Always clear existing breakpoints before setting new ones.
        match target_core.clear_breakpoints(BreakpointType::InstructionBreakpoint) {
            Ok(_) => {}
            Err(error) => tracing::warn!("Failed to clear instruction breakpoints. {}", error),
        }

        let instruction_breakpoint_body = SetInstructionBreakpointsResponseBody {
            breakpoints: arguments
                .breakpoints
                .into_iter()
                .map(|requested_breakpoint| {
                    set_instruction_breakpoint(requested_breakpoint, target_core)
                })
                .collect(),
        };

        // In addition to the response values, also show a message to users for any breakpoints that could not be verified.
        for breakpoint_response in &instruction_breakpoint_body.breakpoints {
            if !breakpoint_response.verified {
                if let Some(message) = &breakpoint_response.message {
                    self.log_to_console(format!("Warning: {message}"));
                    self.show_message(MessageSeverity::Warning, message.clone());
                }
            }
        }

        self.send_response(request, Ok(Some(instruction_breakpoint_body)))
    }

    pub(crate) fn threads(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        // TODO: Implement actual thread resolution. For now, we just use the core id as the thread id.
        let current_core_status = target_core.core.status()?;
        let mut threads: Vec<Thread> = vec![];
        if self.configuration_is_done() {
            // We can handle this request normally.
            let single_thread = Thread {
                id: target_core.core.id() as i64,
                name: target_core.core_data.target_name.clone(),
            };
            threads.push(single_thread);
            return self.send_response(request, Ok(Some(ThreadsResponseBody { threads })));
        }
        self.send_response::<()>(
            request,
            Err(&DebuggerError::Other(anyhow!(
                "Received request for `threads`, while last known core status was {:?}",
                current_core_status
            ))),
        )
    }

    pub(crate) fn stack_trace(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        match target_core.core.status() {
            Ok(status) => {
                if !status.is_halted() {
                    return self.send_response::<()>(
                        request,
                        Err(&DebuggerError::Other(anyhow!(
                            "Core must be halted before requesting a stack trace"
                        ))),
                    );
                }
            }
            Err(error) => {
                return self.send_response::<()>(request, Err(&DebuggerError::ProbeRs(error)));
            }
        };

        let arguments: StackTraceArguments = get_arguments(self, request)?;

        // If the core is halted, and we have no available strackframes, we can get out of here early.
        if target_core.core_data.stack_frames.is_empty() {
            let body = StackTraceResponseBody {
                stack_frames: Vec::new(),
                total_frames: Some(0),
            };
            return self.send_response(request, Ok(Some(body)));
        }

        // The DAP spec says that the `levels` is optional if `None` or `Some(0)`, then all available frames should be returned.
        let mut levels = arguments.levels.unwrap_or(0);
        // The DAP spec says that the `startFrame` is optional and should be 0 if not specified.
        let start_frame = arguments.start_frame.unwrap_or(0);

        // Update the `levels` to the number of available frames if it is 0.
        if levels == 0 {
            levels = target_core.core_data.stack_frames.len() as i64;
        }

        // Determine the correct 'slice' of available [StackFrame]s to serve up ...
        let total_frames = target_core.core_data.stack_frames.len() as i64;

        // We need to copy some parts of StackFrame so that we can re-use it later without references to target_core.
        struct PartialStackFrameData {
            id: ObjectRef,
            function_name: String,
            source_location: Option<SourceLocation>,
            pc: RegisterValue,
            is_inlined: bool,
        }

        let frame_set = if levels == 1 && start_frame == 0 {
            // Just the first frame - use the LHS of the split at `levels`
            target_core
                .core_data
                .stack_frames
                .split_at(levels as usize)
                .0
        } else if total_frames <= 20 && start_frame >= 0 && start_frame <= total_frames {
            // When we have less than 20 frames - use the RHS of of the split at `start_frame`
            target_core
                .core_data
                .stack_frames
                .split_at(start_frame as usize)
                .1
        } else if total_frames > 20 && start_frame + levels <= total_frames {
            // When we have more than 20 frames - we can safely split twice
            target_core
                .core_data
                .stack_frames
                .split_at(start_frame as usize)
                .1
                .split_at(levels as usize)
                .0
        } else if total_frames > 20 && start_frame + levels > total_frames {
            // The MS DAP spec may also ask for more frames than what we reported.
            target_core
                .core_data
                .stack_frames
                .split_at(start_frame as usize)
                .1
        } else {
            return self.send_response::<()>(
                request,
                Err(&DebuggerError::Other(anyhow!(
                    "Request for stack trace failed with invalid arguments: {:?}",
                    arguments
                ))),
            );
        }
        .iter()
        .map(|stack_frame| PartialStackFrameData {
            id: stack_frame.id,
            function_name: stack_frame.function_name.clone(),
            source_location: stack_frame.source_location.clone(),
            pc: stack_frame.pc,
            is_inlined: stack_frame.is_inlined,
        })
        .collect::<Vec<PartialStackFrameData>>();

        let frame_list: Vec<StackFrame> = frame_set
            .iter()
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

                let line = frame
                    .source_location
                    .as_ref()
                    .and_then(|sl| sl.line)
                    .unwrap_or(0) as i64;

                let function_display_name = if frame.is_inlined {
                    format!("{} #[inline]", frame.function_name)
                } else {
                    frame.function_name.clone()
                };

                // Create the appropriate [`dap_types::Source`] for the response
                let source = if let Some(source_location) = &frame.source_location {
                    get_dap_source(source_location)
                } else {
                    tracing::debug!("No source location present for frame!");
                    None
                };

                // TODO: Can we add more meaningful info to `module_id`, etc.
                StackFrame {
                    id: frame.id.into(),
                    name: function_display_name,
                    source,
                    line,
                    column: column as i64,
                    end_column: None,
                    end_line: None,
                    module_id: None,
                    presentation_hint: Some("normal".to_owned()),
                    can_restart: Some(false),
                    instruction_pointer_reference: Some(format!("{}", frame.pc)),
                }
            })
            .collect();

        let body = StackTraceResponseBody {
            stack_frames: frame_list,
            total_frames: Some(total_frames),
        };
        self.send_response(request, Ok(Some(body)))
    }

    /// Retrieve available scopes
    /// - static scope  : Variables with `static` modifier
    /// - registers     : The [probe_rs::Core::registers] for the target [probe_rs::CoreType]
    /// - local scope   : Variables defined between start of current frame, and the current pc (program counter)
    pub(crate) fn scopes(&mut self, target_core: &mut CoreHandle, request: &Request) -> Result<()> {
        let arguments: ScopesArguments = get_arguments(self, request)?;

        let mut dap_scopes: Vec<Scope> = vec![];

        if let Some(core_peripherals) = &target_core.core_data.core_peripherals {
            let peripherals_root_variable = core_peripherals.svd_variable_cache.root_variable_key();
            dap_scopes.push(Scope {
                line: None,
                column: None,
                end_column: None,
                end_line: None,
                expensive: true, // VSCode won't open this tree by default.
                indexed_variables: None,
                name: "Peripherals".to_string(),
                presentation_hint: Some("registers".to_string()),
                named_variables: None,
                source: None,
                variables_reference: peripherals_root_variable.into(),
            });
        };

        if let Some(static_root_variable) = target_core
            .core_data
            .static_variables
            .as_ref()
            .map(|stack_frame| stack_frame.root_variable())
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
                variables_reference: static_root_variable.variable_key().into(),
            });
        };

        let frame_id: ObjectRef = arguments.frame_id.into();

        tracing::trace!("Getting scopes for frame {:?}", frame_id);

        if let Some(stack_frame) = target_core.get_stackframe(frame_id) {
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
                variables_reference: stack_frame.id.into(),
            });

            if let Some(locals_root_variable) = stack_frame
                .local_variables
                .as_ref()
                .map(|stack_frame| stack_frame.root_variable())
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
                    variables_reference: locals_root_variable.variable_key().into(),
                });
            }
        }
        self.send_response(request, Ok(Some(ScopesResponseBody { scopes: dap_scopes })))
    }

    /// Attempt to extract disassembled source code to supply the instruction_count required.
    pub(crate) fn get_disassembled_source(
        &mut self,
        target_core: &mut CoreHandle,
        // The program_counter where our desired instruction range is based.
        memory_reference: i64,
        // The number of bytes offset from the memory reference. Can be zero.
        byte_offset: i64,
        // The number of instruction offset from the memory reference. Can be zero.
        instruction_offset: i64,
        // The EXACT number of instructions to return in the result.
        instruction_count: i64,
    ) -> Result<Vec<dap_types::DisassembledInstruction>, DebuggerError> {
        let assembly_lines = disassemble_target_memory(
            target_core,
            instruction_offset,
            byte_offset,
            memory_reference as u64,
            instruction_count,
        )?;

        if assembly_lines.is_empty() {
            Err(DebuggerError::Other(anyhow::anyhow!(
                "No valid instructions found at memory reference {:#010x?}",
                memory_reference
            )))
        } else {
            Ok(assembly_lines)
        }
    }

    /// Implementing the MS DAP for `request Disassemble` has a number of problems:
    /// - The api requires that we return EXACTLY the instruction_count specified.
    ///   - From testing, if we provide slightly fewer or more instructions, the current versions of VSCode will behave in unpredictable ways (frequently causes runaway renderer processes).
    /// - They provide an instruction offset, which we have to convert into bytes. Some architectures use variable length instructions, so the conversion is inexact.
    /// - They request a fix number of instructions, without regard for whether the memory range is valid.
    ///
    /// To overcome these challenges, we will do the following:
    /// - Calculate the starting point of the memory range based on the architecture's minimum address size.
    /// - Read 4 bytes into a buffer.
    /// - Use [`capstone::Capstone`] to convert 1 instruction from these 4 bytes.
    /// - Subtract the instruction's bytes from our own read buffer.
    /// - Continue this process until we have:
    ///   - Reached the required number of instructions.
    ///   - We encounter 'unreadable' memory on the target.
    ///     - In this case, pad the results with, as the api requires, "implementation defined invalid instructions"
    pub(crate) fn disassemble(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        let arguments: DisassembleArguments = get_arguments(self, request)?;

        if let Ok(memory_reference) = if arguments.memory_reference.starts_with("0x")
            || arguments.memory_reference.starts_with("0X")
        {
            u32::from_str_radix(&arguments.memory_reference[2..], 16)
        } else {
            arguments.memory_reference.parse()
        } {
            match self.get_disassembled_source(
                target_core,
                memory_reference as i64,
                arguments.offset.unwrap_or(0_i64),
                arguments.instruction_offset.unwrap_or(0_i64),
                arguments.instruction_count,
            ) {
                Ok(disassembled_instructions) => self.send_response(
                    request,
                    Ok(Some(DisassembleResponseBody {
                        instructions: disassembled_instructions,
                    })),
                ),
                Err(error) => {
                    self.send_response::<()>(request, Err(&DebuggerError::Other(anyhow!(error))))
                }
            }
        } else {
            self.send_response::<()>(
                request,
                Err(&DebuggerError::Other(anyhow!(
                    "Invalid memory reference {:?}",
                    arguments.memory_reference
                ))),
            )
        }
    }

    /// The MS DAP Specification only gives us the unique reference of the variable, and does not tell us which StackFrame it belongs to,
    /// nor does it specify if this variable is in the local, register or static scope.
    /// Unfortunately this means we have to search through all the available [`probe_rs::debug::variable_cache::VariableCache`]'s until we find it.
    /// To minimize the impact of this, we will search in the most 'likely' places first (first stack frame's locals, then statics, then registers, then move to next stack frame, and so on ...)
    pub(crate) fn variables(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        let arguments: VariablesArguments = get_arguments(self, request)?;

        let variable_ref: ObjectRef = arguments.variables_reference.into();

        // First we check the SVD VariableCache, we do this first because it is the lowest computational overhead.
        if let Some(svd_cache) = target_core
            .core_data
            .core_peripherals
            .as_ref()
            .map(|cp| &cp.svd_variable_cache)
        {
            if svd_cache.get_variable_by_key(variable_ref).is_some() {
                let dap_variables: Vec<Variable> = svd_cache
                    .get_children(variable_ref)
                    .iter()
                    // Convert the `probe_rs::debug::Variable` to `probe_rs_debugger::dap_types::Variable`
                    .map(|variable| {
                        let (variables_reference, named_child_variables_cnt) =
                            get_svd_variable_reference(variable, svd_cache);

                        // We use fully qualified Peripheral.Register.Field form to ensure the `evaluate` request can find the right registers and fields by name.
                        let name = if let Some(last_part) =
                            variable.name().split_terminator('.').next_back()
                        {
                            last_part.to_string()
                        } else {
                            variable.name().to_string()
                        };

                        Variable {
                            name,
                            evaluate_name: Some(variable.name().to_string()),
                            memory_reference: variable.memory_reference(),
                            indexed_variables: None,
                            named_variables: Some(named_child_variables_cnt),
                            presentation_hint: None,
                            type_: variable.type_name(),
                            value: {
                                // The SVD cache is not automatically refreshed on every stack trace, and we only need to refresh the field values.
                                variable.get_value(&mut target_core.core)
                            },
                            variables_reference: variables_reference.into(),
                        }
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

        let mut parent_variable: Option<probe_rs_debug::Variable> = None;
        let mut variable_cache: Option<&mut probe_rs_debug::VariableCache> = None;
        let mut frame_info: Option<StackFrameInfo<'_>> = None;

        let registers;

        if let Some(search_cache) = &mut target_core.core_data.static_variables {
            if let Some(search_variable) = search_cache.get_variable_by_key(variable_ref) {
                parent_variable = Some(search_variable);
                variable_cache = Some(search_cache);

                if let Some(top_level_frame) = target_core.core_data.stack_frames.first() {
                    registers = top_level_frame.registers.clone();

                    frame_info = Some(StackFrameInfo {
                        registers: &registers,
                        frame_base: top_level_frame.frame_base,
                        canonical_frame_address: top_level_frame.canonical_frame_address,
                    });
                }
            }
        }

        if parent_variable.is_none() {
            for stack_frame in target_core.core_data.stack_frames.iter_mut() {
                if let Some(search_cache) = &mut stack_frame.local_variables {
                    if let Some(search_variable) = search_cache.get_variable_by_key(variable_ref) {
                        parent_variable = Some(search_variable);
                        variable_cache = Some(search_cache);
                        frame_info = Some(StackFrameInfo {
                            registers: &stack_frame.registers,
                            frame_base: stack_frame.frame_base,
                            canonical_frame_address: stack_frame.canonical_frame_address,
                        });
                        break;
                    }
                }

                if stack_frame.id == variable_ref {
                    // This is a special case, where we just want to return the stack frame registers.

                    let dap_variables: Vec<Variable> = stack_frame
                        .registers
                        .0
                        .iter()
                        .map(|register| Variable {
                            name: register.get_register_name(),
                            evaluate_name: Some(register.get_register_name()),
                            memory_reference: None,
                            indexed_variables: None,
                            named_variables: None,
                            presentation_hint: None, // TODO: Implement hint as Hex for registers
                            type_: Some(format!("{}", VariableName::RegistersRoot)),
                            value: register.value.unwrap_or_default().to_string(),
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
        }

        // During the intial stack unwind operation, if encounter [Variable]'s with [VariableNodeType::is_deferred()], they will not be auto-expanded and included in the variable cache.
        // TODO: Use the DAP "Invalidated" event to refresh the variables for this stackframe. It will allow the UI to see updated compound values for pointer variables based on the newly resolved children.
        if let Some(variable_cache) = variable_cache {
            if let Some(parent_variable) = parent_variable.as_mut() {
                if parent_variable.variable_node_type.is_deferred()
                    && !variable_cache.has_children(parent_variable)
                {
                    if let Some(frame_info) = frame_info {
                        target_core.core_data.debug_info.cache_deferred_variables(
                            variable_cache,
                            &mut target_core.core,
                            parent_variable,
                            frame_info,
                        )?;
                    } else {
                        tracing::error!(
                            "Could not cache deferred child variables for variable: {}. No register data available.",
                            parent_variable.name
                        );
                    }
                }
            }

            let dap_variables: Vec<Variable> = variable_cache
                .get_children(variable_ref)
                // Filter out requested children, then map them as DAP variables
                .filter(|variable| match &arguments.filter {
                    Some(filter) => match filter.as_str() {
                        "indexed" => variable.is_indexed(),
                        "named" => !variable.is_indexed(),
                        other => {
                            // This will yield an empty Vec, which will result in a user facing error as well as the log below.
                            tracing::error!("Received invalid variable filter: {}", other);
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
                    ) = get_variable_reference(variable, variable_cache);
                    Variable {
                        name: variable.name.to_string(),
                        // evaluate_name: Some(variable.name.to_string()),
                        // Do NOT use evaluate_name. It is impossible to distinguish between duplicate variable
                        // TODO: Implement qualified names.
                        evaluate_name: None,
                        memory_reference: Some(variable.memory_location.to_string()),
                        indexed_variables: Some(indexed_child_variables_cnt),
                        named_variables: Some(named_child_variables_cnt),
                        presentation_hint: None,
                        type_: Some(variable.type_name()),
                        value: variable.to_string(variable_cache),
                        variables_reference: variables_reference.into(),
                    }
                })
                .collect();
            self.send_response(
                request,
                Ok(Some(VariablesResponseBody {
                    variables: dap_variables,
                })),
            )
        } else {
            let err = DebuggerError::Other(anyhow!(
                "No variable information found for {}!",
                arguments.variables_reference
            ));

            let res: Result<Option<u32>, _> = Err(&err);

            self.send_response(request, res)
        }
    }

    pub(crate) fn r#continue(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        if let Err(error) = target_core.core.run() {
            self.send_response::<()>(request, Err(&DebuggerError::Other(anyhow!("{}", error))))?;
            return Err(error.into());
        }

        target_core.reset_core_status(self);

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

        // If there are breakpoints configured, we wait a bit longer
        let wait_timeout = if target_core.core_data.breakpoints.is_empty() {
            Duration::from_millis(200)
        } else {
            Duration::from_millis(500)
        };

        tracing::trace!("Checking if core halts again after continue, timeout = {wait_timeout:?}");

        match target_core.core.wait_for_core_halted(wait_timeout) {
            // The core has halted, so we can proceed.
            Ok(_) => Ok(()),
            // The core is still running.
            Err(
                Error::Arm(ArmError::Timeout)
                | Error::Riscv(RiscvError::Timeout)
                | Error::Xtensa(XtensaError::Timeout),
            ) => Ok(()),
            // Some other error occurred, so we have to send an error response.
            Err(wait_error) => Err(wait_error.into()),
        }
    }

    /// Steps through the code at the requested granularity.
    /// - [SteppingMode::StepInstruction]: If MS DAP [SteppingGranularity::Instruction] (usually sent from the disassembly view)
    /// - [SteppingMode::OverStatement]: In all other cases.
    pub(crate) fn next(&mut self, target_core: &mut CoreHandle, request: &Request) -> Result<()> {
        let arguments: NextArguments = get_arguments(self, request)?;

        let stepping_granularity = match arguments.granularity {
            Some(SteppingGranularity::Instruction) => SteppingMode::StepInstruction,
            _ => SteppingMode::OverStatement,
        };

        self.debug_step(stepping_granularity, target_core, request)
    }

    /// Steps through the code at the requested granularity.
    /// - [SteppingMode::StepInstruction]: If MS DAP [SteppingGranularity::Instruction] (usually sent from the disassembly view)
    /// - [SteppingMode::IntoStatement]: In all other cases.
    pub(crate) fn step_in(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        let arguments: StepInArguments = get_arguments(self, request)?;

        let stepping_granularity = match arguments.granularity {
            Some(SteppingGranularity::Instruction) => SteppingMode::StepInstruction,
            _ => SteppingMode::IntoStatement,
        };
        self.debug_step(stepping_granularity, target_core, request)
    }

    /// Steps through the code at the requested granularity.
    /// - [SteppingMode::StepInstruction]: If MS DAP [SteppingGranularity::Instruction] (usually sent from the disassembly view)
    /// - [SteppingMode::OutOfStatement]: In all other cases.
    pub(crate) fn step_out(
        &mut self,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<()> {
        let arguments: StepOutArguments = get_arguments(self, request)?;

        let stepping_granularity = match arguments.granularity {
            Some(SteppingGranularity::Instruction) => SteppingMode::StepInstruction,
            _ => SteppingMode::OutOfStatement,
        };

        self.debug_step(stepping_granularity, target_core, request)
    }

    /// Common code for the `next`, `step_in`, and `step_out` methods.
    fn debug_step(
        &mut self,
        stepping_granularity: SteppingMode,
        target_core: &mut CoreHandle,
        request: &Request,
    ) -> Result<(), anyhow::Error> {
        target_core.reset_core_status(self);
        let (new_status, program_counter) = match stepping_granularity
            .step(&mut target_core.core, &target_core.core_data.debug_info)
        {
            Ok((new_status, program_counter)) => (new_status, program_counter),
            Err(probe_rs_debug::DebugError::WarnAndContinue { message }) => {
                let pc_at_error = target_core
                    .core
                    .read_core_reg(target_core.core.program_counter())?;
                self.show_message(
                    MessageSeverity::Information,
                    format!("Step error @{pc_at_error:#010X}: {message}"),
                );
                (target_core.core.status()?, pc_at_error)
            }
            Err(other_error) => {
                target_core.core.halt(Duration::from_millis(100)).ok();
                return Err(other_error).context("Unexpected error during stepping");
            }
        };

        self.send_response::<()>(request, Ok(None))?;

        // We override the halt reason because our implementation of stepping uses breakpoints and results in a "BreakPoint" halt reason, which is not appropriate here.
        target_core.core_data.last_known_status = CoreStatus::Halted(HaltReason::Step);
        if matches!(new_status, CoreStatus::Halted(_)) {
            let event_body = Some(StoppedEventBody {
                reason: target_core
                    .core_data
                    .last_known_status
                    .short_long_status(None)
                    .0
                    .to_string(),
                description: Some(
                    CoreStatus::Halted(HaltReason::Step)
                        .short_long_status(Some(program_counter))
                        .1,
                ),
                thread_id: Some(target_core.core.id() as i64),
                preserve_focus_hint: None,
                text: None,
                all_threads_stopped: Some(self.all_cores_halted),
                hit_breakpoint_ids: None,
            });
            self.send_event("stopped", event_body)?;
        }
        Ok(())
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
    pub fn send_response<S: Serialize + std::fmt::Debug>(
        &mut self,
        request: &Request,
        response: Result<Option<S>, &DebuggerError>,
    ) -> Result<()> {
        self.adapter.send_response(request, response)
    }

    /// Displays an error message to the user.
    pub fn show_error_message(&mut self, response: &DebuggerError) -> Result<()> {
        let expanded_error = {
            let mut response_message = response.to_string();
            let mut offset_iterations = 0;
            let mut child_error: Option<&dyn std::error::Error> =
                std::error::Error::source(&response);
            while let Some(source_error) = child_error {
                offset_iterations += 1;
                response_message = format!("{response_message}\n",);
                for _offset_counter in 0..offset_iterations {
                    response_message = format!("{response_message}\t");
                }
                response_message = format!(
                    "{}{}",
                    response_message,
                    <dyn std::error::Error>::to_string(source_error)
                );
                child_error = std::error::Error::source(source_error);
            }
            response_message
        };
        if self
            .adapter
            .show_message(MessageSeverity::Error, expanded_error)
        {
            Ok(())
        } else {
            Err(anyhow!("Failed to send error response"))
        }
    }

    #[tracing::instrument(level = "trace", skip_all)]
    pub fn send_event<S: Serialize>(
        &mut self,
        event_type: &str,
        event_body: Option<S>,
    ) -> Result<()> {
        tracing::debug!("Sending event: {}", event_type);
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
        channel_number: u32,
        channel_name: String,
        data_format: rtt::DataFormat,
    ) -> bool {
        let Ok(event_body) = serde_json::to_value(RttChannelEventBody {
            channel_number,
            channel_name,
            data_format,
        }) else {
            return false;
        };

        self.send_event("probe-rs-rtt-channel-config", Some(event_body))
            .is_ok()
    }

    /// Send a custom `probe-rs-rtt-data` event to the MS DAP Client, to
    pub fn rtt_output(&mut self, channel_number: u32, rtt_data: String) -> bool {
        let Ok(event_body) = serde_json::to_value(RttDataEventBody {
            channel_number,
            data: rtt_data,
        }) else {
            return false;
        };

        self.send_event("probe-rs-rtt-data", Some(event_body))
            .is_ok()
    }

    fn new_progress_id(&mut self) -> ProgressId {
        let id = self.progress_id;

        self.progress_id += 1;

        id
    }

    pub fn start_progress(
        &mut self,
        title: &str,
        request_id: Option<ProgressId>,
    ) -> Result<ProgressId> {
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
        progress: Option<f64>,
        message: Option<impl Display>,
        progress_id: i64,
    ) -> Result<ProgressId> {
        anyhow::ensure!(
            self.supports_progress_reporting,
            "Progress reporting is not supported by client."
        );

        let percentage = progress.map(|progress| progress * 100.0);

        self.send_event(
            "progressUpdate",
            Some(ProgressUpdateEventBody {
                message: message.map(|msg| match percentage {
                    None => msg.to_string(),
                    Some(100.0) => msg.to_string(),
                    Some(percentage) => format!("{msg} ({percentage:02.0}%)"),
                }),
                percentage,
                progress_id: progress_id.to_string(),
            }),
        )?;

        Ok(progress_id)
    }

    pub(crate) fn set_console_log_level(&mut self, error: ConsoleLog) {
        self.adapter.set_console_log_level(error)
    }
}

pub fn get_arguments<T: DeserializeOwned, P: ProtocolAdapter>(
    debug_adapter: &mut DebugAdapter<P>,
    req: &Request,
) -> Result<T, DebuggerError> {
    let Some(raw_arguments) = &req.arguments else {
        debug_adapter.send_response::<()>(req, Err(&DebuggerError::InvalidRequest))?;
        return Err(DebuggerError::Other(anyhow!(
            "Failed to get {} arguments",
            req.command
        )));
    };

    match serde_json::from_value(raw_arguments.to_owned()) {
        Ok(value) => Ok(value),
        Err(e) => {
            debug_adapter.send_response::<()>(req, Err(&e.into()))?;
            Err(DebuggerError::Other(anyhow!(
                "Failed to deserialize {} arguments",
                req.command
            )))
        }
    }
}
