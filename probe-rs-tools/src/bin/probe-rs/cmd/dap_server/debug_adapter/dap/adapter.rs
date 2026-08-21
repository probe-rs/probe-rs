use super::{
    core_status::DapStatus,
    dap_types,
    repl_commands_helpers::{build_expanded_commands, command_completions},
    request_helpers::{get_dap_source, instruction_breakpoint_response},
};
use crate::cmd::dap_server::backend::rpc::RpcBackend;
use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::{
        dap::repl_commands::{EvalResponse, EvalResult, ReplCommand},
        protocol::{BoxedAdapter, ProtocolAdapter, ProtocolHelper, hide_file_payloads},
    },
    server::{
        configuration::ConsoleLog,
        core_data::CoreData,
        session_data::{ActiveBreakpoint, BreakpointType, SessionData, SourceLocationScope},
    },
};
use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose as base64_engine};
use dap_types::*;
use parse_int::parse;
use probe_rs::{
    Architecture, CoreInformation, CoreRegister, CoreStatus, HaltReason, RegisterDataType,
    RegisterRole, RegisterValue, UnwindRule,
};
use probe_rs_debug::{
    ColumnType, ObjectRef, SourceLocation, SteppingMode, VerifiedBreakpoint,
    registers::{DebugRegister, DebugRegisters},
};
use probe_rs_rpc::breakpoints::SourceBreakpointLocation;
use probe_rs_rpc::rtt_config::DataFormat;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use typed_path::NativePathBuf;

use std::{fmt::Display, str, time::Duration};

/// Progress ID used for progress reporting when the debug adapter protocol is used.
pub(crate) type ProgressId = i64;

#[derive(Debug, PartialEq, Eq)]
enum EvaluateDispatch {
    Server,
    ReplCommand,
    Unsupported,
}

fn evaluate_dispatch(context: Option<&str>) -> EvaluateDispatch {
    match context {
        Some("watch" | "hover") => EvaluateDispatch::Server,
        Some("repl") => EvaluateDispatch::ReplCommand,
        _ => EvaluateDispatch::Unsupported,
    }
}

/// A Debug Adapter Protocol "Debug Adapter",
/// see <https://microsoft.github.io/debug-adapter-protocol/overview>
pub struct DebugAdapter {
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
    /// Flag to indicate if the connected client can render ANSI escape sequences in
    /// `OutputEvent.output` and evaluate responses. Populated from the
    /// `supportsAnsiStyling` field of the `initialize` request.
    pub(crate) supports_ansi_styling: bool,
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
    adapter: BoxedAdapter,
}

impl DebugAdapter {
    pub fn new(adapter: impl ProtocolAdapter + Send + 'static) -> DebugAdapter {
        DebugAdapter {
            vscode_quirks: false,
            halt_after_reset: false,
            configuration_done: false,
            all_cores_halted: true,
            progress_id: 0,
            supports_progress_reporting: false,
            supports_ansi_styling: false,
            lines_start_at_1: true,
            columns_start_at_1: true,
            adapter: Box::new(adapter),
        }
    }

    pub(crate) fn configuration_is_done(&self) -> bool {
        self.configuration_done
    }

    pub(crate) async fn pause(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let response = match self
            .pause_impl_async(
                &mut session_data.backend,
                session_data
                    .core_data
                    .iter_mut()
                    .find(|c| c.core_index == core_index)
                    .ok_or_else(|| {
                        DebuggerError::Other(anyhow!("No core data for core {core_index}"))
                    })?,
            )
            .await
        {
            Ok(cpu_info) => Ok(Some(format!(
                "Core stopped at address {:#010x}",
                cpu_info.pc
            ))),
            Err(error) => Err(&DebuggerError::Other(anyhow!("{error}"))),
        };

        self.send_response(request, response)
    }

    pub(crate) async fn disconnect(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let arguments: DisconnectArguments = get_arguments(self, request)?;

        let must_halt_debuggee = arguments.terminate_debuggee.unwrap_or(false)
            || arguments.suspend_debuggee.unwrap_or(false);

        if must_halt_debuggee {
            let _ = session_data
                .backend
                .halt(core_index, Duration::from_millis(100))
                .await;
        }

        self.send_response::<DisconnectResponse>(request, Ok(None))
    }

    pub(crate) async fn read_memory(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let arguments: ReadMemoryArguments = get_arguments(self, request)?;

        let memory_offset = arguments.offset.unwrap_or(0);
        let address: u64 = match parse::<u64>(arguments.memory_reference.as_ref()) {
            Ok(address) => address.wrapping_add(memory_offset as u64), // handles negative offsets
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
        let result_buffer = session_data
            .backend
            .read_bytes(core_index, address, arguments.count as usize)
            .await
            .unwrap_or_default();
        let num_bytes_unread = arguments.count as usize - result_buffer.len();
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
                    "Could not read any data at address {address:#010x}"
                ))),
            )
        }
    }

    pub(crate) async fn write_memory(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let arguments: WriteMemoryArguments = get_arguments(self, request)?;
        let memory_offset = arguments.offset.unwrap_or(0);
        let address: u64 = match parse::<u64>(arguments.memory_reference.as_ref()) {
            Ok(address) => address.wrapping_add(memory_offset as u64), // handles negative offsets
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

        if let Err(error) = session_data
            .backend
            .write_memory_8(core_index, address, data_bytes.clone())
            .await
        {
            return self.send_response::<()>(request, Err(&DebuggerError::ProbeRs(error)));
        }

        self.send_response(
            request,
            Ok(Some(WriteMemoryResponseBody {
                bytes_written: Some(data_bytes.len() as i64),
                offset: None,
            })),
        )?;
        // TODO: This doesn't trigger the VSCode UI to reload the variables affected.
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

    /// Resolve scopes through RPC from the server-owned debug state.
    pub(crate) async fn scopes(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let arguments: ScopesArguments = get_arguments(self, request)?;
        let frame_id = arguments.frame_id as u32;
        let scopes = session_data
            .backend
            .scopes(core_index, frame_id)
            .await
            .map_err(DebuggerError::ProbeRs)?;
        self.send_response(request, Ok(Some(ScopesResponseBody { scopes })))
    }

    /// Resolve variables through RPC from the server-owned caches.
    pub(crate) async fn variables(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let arguments: VariablesArguments = get_arguments(self, request)?;
        let variables_reference = arguments.variables_reference as u32;
        let variables = session_data
            .backend
            .variables(core_index, variables_reference, arguments.filter)
            .await
            .map_err(DebuggerError::ProbeRs)?;
        self.send_response(request, Ok(Some(VariablesResponseBody { variables })))
    }

    /// Route watch/hover and non-command REPL expressions to the server-owned
    /// evaluator. Registered REPL commands remain adapter-side dispatch, but
    /// their target operations use the RPC backend.
    pub(crate) async fn evaluate(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let arguments: EvaluateArguments = get_arguments(self, request)?;

        match evaluate_dispatch(arguments.context.as_deref()) {
            EvaluateDispatch::Server => {
                let response_body = session_data
                    .backend
                    .evaluate(core_index, &arguments)
                    .await
                    .map_err(DebuggerError::ProbeRs)?;
                self.send_response(request, Ok(Some(response_body)))
            }
            EvaluateDispatch::ReplCommand => {
                let mut response_body = EvaluateResponseBody {
                    indexed_variables: None,
                    memory_reference: None,
                    named_variables: None,
                    presentation_hint: None,
                    result: format!("<invalid expression {:?}>", arguments.expression),
                    type_: None,
                    variables_reference: 0,
                    value_location_reference: None,
                };
                match self.handle_repl(session_data, core_index, &arguments).await {
                    Ok(EvalResponse::Body(body)) => response_body = body,
                    Ok(EvalResponse::Message(message)) => response_body.result = message,
                    Err(DebuggerError::UserMessage(message)) => response_body.result = message,
                    Err(error) => response_body.result = error.to_string(),
                }
                self.send_response(request, Ok(Some(response_body)))
            }
            EvaluateDispatch::Unsupported => {
                let context = arguments.context.as_deref().unwrap_or("<missing>");
                let error = DebuggerError::UserMessage(format!(
                    "Evaluate context {context:?} is not supported."
                ));
                self.send_response::<EvaluateResponseBody>(request, Err(&error))
            }
        }
    }

    async fn handle_repl(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        arguments: &EvaluateArguments,
    ) -> EvalResult {
        let expression_trimmed = arguments.expression.trim();
        let (command_root, last_piece, repl_commands) = {
            let Some(cd) = session_data
                .core_data
                .iter()
                .find(|c| c.core_index == core_index)
            else {
                return Err(DebuggerError::UserMessage(
                    "No core data for core".to_string(),
                ));
            };
            build_expanded_commands(&cd.repl_commands, expression_trimmed)
        };

        let Some(repl_command) = repl_commands.first() else {
            return Err(DebuggerError::UserMessage(format!(
                "Unknown REPL command: {expression_trimmed}."
            )));
        };
        let repl_command = *repl_command;

        if repl_command.requires_target_halted
            && !session_data.backend.core_halted(core_index).await?
        {
            return Err(DebuggerError::UserMessage(
                "The target is running. Only the 'break', 'help' or 'quit' commands are allowed."
                    .to_string(),
            ));
        }

        let argument_string = arguments
            .expression
            .trim_start_matches(&command_root)
            .trim_start()
            .trim_start_matches(last_piece)
            .trim_start();

        self.dispatch_repl_command(
            repl_command,
            session_data,
            core_index,
            argument_string,
            arguments,
        )
        .await
    }

    /// Async REPL dispatch: resolves the core's [`CoreData`] and hands off to
    /// the [`ReplCommand`] handler, which `.await`s the backend directly.
    async fn dispatch_repl_command(
        &mut self,
        leaf: ReplCommand,
        session_data: &mut SessionData,
        core_index: usize,
        argument_string: &str,
        arguments: &EvaluateArguments,
    ) -> EvalResult {
        let core_data = session_data
            .core_data
            .iter_mut()
            .find(|c| c.core_index == core_index)
            .ok_or_else(|| DebuggerError::Other(anyhow!("No core data for core {core_index}")))?;
        (leaf.handler)(
            &mut session_data.backend,
            core_data,
            argument_string,
            arguments,
            self,
        )
        .await
    }

    /// Works in tandem with the `evaluate` request, to provide possible completions in the Debug Console REPL window.
    pub(crate) async fn rtt_window_opened(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let arguments: RttWindowOpenedArguments = get_arguments(self, request)?;

        if let Some(debugger_rtt_target) = session_data
            .core_data
            .iter_mut()
            .find(|cd| cd.core_index == core_index)
            .and_then(|cd| cd.rtt_connection.as_mut())
            && let Some(rtt_channel) =
                debugger_rtt_target
                    .debugger_rtt_channels
                    .iter_mut()
                    .find(|debugger_rtt_channel| {
                        debugger_rtt_channel.channel_number == arguments.channel_number
                    })
        {
            rtt_channel.has_client_window = arguments.window_is_open;
        }

        self.send_response::<()>(request, Ok(None))
    }

    /// Works in tandem with the `evaluate` request, to provide possible completions in the Debug Console REPL window.
    pub(crate) async fn completions(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let arguments: CompletionsArguments = get_arguments(self, request)?;

        let repl_commands = session_data
            .core_data_opt(core_index)
            .map(|cd| &cd.repl_commands);
        let response_body = CompletionsResponseBody {
            targets: repl_commands
                .map(|rc| command_completions(rc, arguments))
                .unwrap_or_default(),
        };

        self.send_response(request, Ok(Some(response_body)))
    }

    /// Set the variable with the given name in the variable container to a new value.
    pub(crate) async fn set_variable(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
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
            memory_reference: None,
            value_location_reference: None,
        };

        // The arguments.variables_reference contains the reference of the variable container. This can be:
        // - The `StackFrame.id` for register variables.
        // - The `Variable.parent_key` for a local or static variable - If these are base data types, we will attempt to update their value, otherwise we will warn the user that updating complex / structure variables are not yet supported.
        let parent_key: ObjectRef = arguments.variables_reference.into();
        let new_value = arguments.value.clone();

        let Some(cd_idx) = session_data
            .core_data
            .iter()
            .position(|cd| cd.core_index == core_index)
        else {
            return self.send_response::<SetVariableResponseBody>(
                request,
                Err(&DebuggerError::Other(anyhow!(
                    "No core data for core {core_index}"
                ))),
            );
        };

        // TODO: Check for, and prevent SVD Peripheral/Register/Field values from being updated, until such time as we can do it safely.

        let register_path = session_data.core_data[cd_idx]
            .stack_frames
            .iter()
            .position(|stack_frame| stack_frame.id == parent_key);

        if let Some(stack_frame_index) = register_path {
            // The variable is a register value in this StackFrame. Only the top frame maps to
            // actual core registers; older frames are reconstructed by unwinding.
            let is_top_stack_frame = session_data.core_data[cd_idx]
                .stack_frames
                .first()
                .is_some_and(|stack_frame| stack_frame.id == parent_key);
            if !is_top_stack_frame {
                return self.send_response::<SetVariableResponseBody>(
                    request,
                    Err(&DebuggerError::Other(anyhow!(
                        "Register writes are only supported for the top stack frame."
                    ))),
                );
            }

            let Some(register) = find_register_by_dap_name(
                &session_data.core_data[cd_idx].stack_frames[stack_frame_index].registers,
                arguments.name.as_str(),
            ) else {
                return self.send_response::<SetVariableResponseBody>(
                    request,
                    Err(&DebuggerError::Other(anyhow!(
                        "Register '{}' was not found in this stack frame.",
                        arguments.name
                    ))),
                );
            };

            let register_name = register.get_register_name();
            let register_id = register.core_register.id;

            let register_value = match parse_register_value(&new_value, register.core_register) {
                Ok(register_value) => register_value,
                Err(error) => {
                    return self.send_response::<SetVariableResponseBody>(
                        request,
                        Err(&DebuggerError::Other(anyhow!(
                            "Failed to parse value for register {register_name}: {error}"
                        ))),
                    );
                }
            };

            match session_data.backend.core_halted(core_index).await {
                Ok(true) => {}
                Ok(false) => {
                    return self.send_response::<SetVariableResponseBody>(
                        request,
                        Err(&DebuggerError::Other(anyhow!(
                            "Register writes require the target core to be halted."
                        ))),
                    );
                }
                Err(error) => {
                    return self.send_response::<SetVariableResponseBody>(
                        request,
                        Err(&DebuggerError::Other(anyhow!(
                            "Failed to read core status before writing register {register_name}: {error}"
                        ))),
                    );
                }
            }

            if let Err(error) = session_data
                .backend
                .write_core_reg(core_index, register_id, register_value)
                .await
            {
                return self.send_response::<SetVariableResponseBody>(
                    request,
                    Err(&DebuggerError::Other(anyhow!(
                        "Failed to write register {register_name}: {error}"
                    ))),
                );
            }

            let written_register_value = match session_data
                .backend
                .read_core_reg(core_index, register_id)
                .await
            {
                Ok(written_register_value) => written_register_value,
                Err(error) => {
                    return self.send_response::<SetVariableResponseBody>(
                        request,
                        Err(&DebuggerError::Other(anyhow!(
                            "Failed to read register {register_name} after writing it: {error}"
                        ))),
                    );
                }
            };

            if register_requires_exact_readback(register.core_register)
                && written_register_value != register_value
            {
                return self.send_response::<SetVariableResponseBody>(
                    request,
                    Err(&DebuggerError::Other(anyhow!(
                        "Register {register_name} read back as {written_register_value} after writing {register_value}."
                    ))),
                );
            }

            if register_write_requires_stack_frame_refresh(register.core_register) {
                // The write has already changed unwind-sensitive target
                // state. Do not expose the old display cache while the
                // server refresh is pending or if it fails.
                session_data.core_data[cd_idx].invalidate_stack_frame_cache();
                match session_data.backend.unwind_stack(core_index, 500).await {
                    Ok(frames) => {
                        // Keep the ids assigned by the authoritative
                        // server cache; scopes/variables resolve those
                        // exact ids on subsequent requests.
                        session_data.core_data[cd_idx].replace_stack_frame_cache(frames);
                    }
                    Err(error) => {
                        let message = format!(
                            "Register {register_name} was written, but stack frames could not be refreshed: {error}"
                        );
                        tracing::warn!("{message}");
                        self.show_message(MessageSeverity::Warning, message);
                    }
                }
            } else if let Some(cached_register) = session_data.core_data[cd_idx]
                .stack_frames
                .get_mut(stack_frame_index)
                .and_then(|f| f.registers.get_register_mut(register_id))
            {
                cached_register.value = Some(written_register_value);
            }

            response_body.indexed_variables = Some(0);
            response_body.named_variables = Some(0);
            response_body.type_ = Some(format!("{:?}", register.core_register.data_type()));
            response_body.value = written_register_value.to_string();
            response_body.variables_reference = Some(0);
        } else {
            // Variable caches are server-owned; every non-register update is
            // resolved and applied by the RPC endpoint.
            match session_data
                .backend
                .set_variable(
                    core_index,
                    parent_key.into(),
                    arguments.name.clone(),
                    new_value.clone(),
                )
                .await
            {
                Ok(resp) => {
                    response_body.value = resp.value;
                    response_body.type_ = resp.type_;
                    response_body.variables_reference = Some(resp.variables_reference);
                    response_body.named_variables = resp.named_variables;
                    response_body.indexed_variables = resp.indexed_variables;
                    response_body.memory_reference = resp.memory_reference;
                }
                Err(error) => {
                    return self.send_response::<SetVariableResponseBody>(
                        request,
                        Err(&DebuggerError::Other(anyhow!(
                            "Failed to update variable: {}, with new value {new_value:?}: {error}",
                            arguments.name,
                        ))),
                    );
                }
            }
        }

        let response = if response_body.value.is_empty() {
            // If we get here, it is a bug.
            Err(&DebuggerError::Other(anyhow!(
                "Failed to update variable: {}, with new value {:?} : Please report this as a bug.",
                arguments.name,
                arguments.value
            )))
        } else {
            Ok(Some(response_body))
        };
        self.send_response(request, response)
    }

    #[tracing::instrument(level = "debug", skip_all, name = "Handle configuration done")]
    pub(crate) async fn configuration_done(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let current_core_status = session_data.backend.status(core_index).await?;

        if current_core_status.is_halted() {
            if self.halt_after_reset
                || matches!(
                    current_core_status,
                    CoreStatus::Halted(HaltReason::Breakpoint(_))
                )
            {
                let program_counter = session_data.backend.program_counter(core_index).await;
                let (reason, description) = current_core_status.short_long_status(program_counter);
                let event_body = Some(StoppedEventBody {
                    reason: reason.to_owned(),
                    description: Some(description),
                    thread_id: Some(core_index as i64),
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
                self.continue_impl_async(
                    &mut session_data.backend,
                    session_data
                        .core_data
                        .iter_mut()
                        .find(|c| c.core_index == core_index)
                        .ok_or_else(|| {
                            DebuggerError::Other(anyhow!("No core data for core {core_index}"))
                        })?,
                )
                .await?;
            }
        }

        self.configuration_done = true;

        self.send_response::<()>(request, Ok(None))
    }

    pub(crate) async fn set_breakpoints(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let args: SetBreakpointsArguments = get_arguments(self, request)?;

        let Some(source_path) = args.source.path.as_ref() else {
            return self.send_response::<()>(
                request,
                Err(&DebuggerError::Other(anyhow!(
                    "Could not get a valid source path from arguments: {args:?}"
                ))),
            );
        };
        let source = args.source.clone();
        let typed_source_path = NativePathBuf::from(source_path).to_typed_path_buf();
        let requested_bps = args.breakpoints.as_deref().unwrap_or_default().to_vec();

        let clear_addrs = {
            let core_data = match session_data.core_data_mut(core_index) {
                Err(error) => return self.send_response::<()>(request, Err(&error)),
                Ok(core_data) => core_data,
            };

            let clear_addrs: Vec<u64> = core_data
                .breakpoints
                .iter()
                .filter(|ab| {
                    matches!(
                        &ab.breakpoint_type,
                        BreakpointType::SourceBreakpoint { source: bp_source, .. }
                            if bp_source.as_ref() == &source
                    )
                })
                .map(|ab| ab.address)
                .collect();
            core_data.breakpoints.retain(|ab| {
                !matches!(
                    &ab.breakpoint_type,
                    BreakpointType::SourceBreakpoint { source: bp_source, .. }
                        if bp_source.as_ref() == &source
                )
            });
            clear_addrs
        };
        let locations = requested_bps
            .iter()
            .map(|bp| {
                let line = if self.lines_start_at_1 {
                    bp.line as u64
                } else {
                    bp.line as u64 + 1
                };
                let column = if self.columns_start_at_1 {
                    Some(bp.column.unwrap_or(1) as u64)
                } else {
                    Some(bp.column.unwrap_or(0) as u64 + 1)
                };
                SourceBreakpointLocation {
                    path: typed_source_path.to_path().display().to_string(),
                    line,
                    column,
                }
            })
            .collect();
        let resolved = match session_data
            .backend
            .resolve_source_breakpoints(locations)
            .await
        {
            Ok(resolved) => resolved
                .into_iter()
                .map(|result| {
                    result.map_err(|error| {
                        format!(
                            "Cannot set breakpoint here. Try reducing compile time-, and link time-, optimization in your build configuration, or choose a different source location: {error}"
                        )
                    })
                })
                .collect::<Vec<Result<VerifiedBreakpoint, String>>>(),
            Err(error) => {
                return self.send_response::<()>(
                    request,
                    Err(&DebuggerError::Other(anyhow!(
                        "Cannot set source breakpoint without debug information: {error}"
                    ))),
                );
            }
        };

        // One round trip to clear the old set, one to set the new set.
        if let Err(error) = session_data
            .backend
            .clear_hw_breakpoints(core_index, clear_addrs)
            .await
        {
            return self.send_response::<()>(
                request,
                Err(&DebuggerError::Other(anyhow!(
                    "Failed to clear existing breakpoints before setting new ones: {error}"
                ))),
            );
        }
        let set_addrs: Vec<u64> = resolved
            .iter()
            .filter_map(|r| r.as_ref().ok().map(|vb| vb.address))
            .collect();
        let set_results = session_data
            .backend
            .set_hw_breakpoints(core_index, set_addrs)
            .await
            .map_err(|e| DebuggerError::Other(anyhow!("Failed to set breakpoints: {e}")))?;

        let mut created_breakpoints: Vec<Breakpoint> = Vec::with_capacity(requested_bps.len());
        let mut to_cache: Vec<(u64, SourceLocation)> = Vec::new();
        let mut set_idx = 0;
        for (i, bp) in requested_bps.iter().enumerate() {
            match &resolved[i] {
                Err(msg) => created_breakpoints.push(Breakpoint {
                    column: None,
                    end_column: None,
                    end_line: None,
                    id: None,
                    line: Some(bp.line),
                    message: Some(msg.clone()),
                    source: None,
                    instruction_reference: None,
                    offset: None,
                    verified: false,
                    reason: Some("failed".to_string()),
                }),
                Ok(VerifiedBreakpoint {
                    address,
                    source_location,
                }) => {
                    let set_result = set_results.get(set_idx);
                    set_idx += 1;
                    if matches!(set_result, Some(Ok(()))) {
                        to_cache.push((*address, source_location.clone()));
                        created_breakpoints.push(Breakpoint {
                            column: source_location.column.map(|col| match col {
                                ColumnType::LeftEdge => 0_i64,
                                ColumnType::Column(c) => c as i64,
                            }),
                            end_column: None,
                            end_line: None,
                            id: Some(*address as i64),
                            line: source_location.line.map(|line| line as i64),
                            message: Some(format!(
                                "Source breakpoint at memory address: {address:#010X}"
                            )),
                            source: Some(source.clone()),
                            instruction_reference: Some(format!("{address:#010X}")),
                            offset: None,
                            verified: true,
                            reason: None,
                        });
                    } else {
                        created_breakpoints.push(Breakpoint {
                            column: None,
                            end_column: None,
                            end_line: None,
                            id: None,
                            line: Some(bp.line),
                            message: Some(match set_result {
                                Some(Err(error)) => format!(
                                    "Failed to set hardware breakpoint at {address:#010X}: {error}"
                                ),
                                _ => {
                                    format!("Failed to set hardware breakpoint at {address:#010X}")
                                }
                            }),
                            source: None,
                            instruction_reference: None,
                            offset: None,
                            verified: false,
                            reason: Some("failed".to_string()),
                        });
                    }
                }
            }
        }

        // Update the client-side breakpoint cache.
        if let Ok(core_data) = session_data.core_data_mut(core_index) {
            for (address, source_location) in to_cache {
                core_data.breakpoints.push(ActiveBreakpoint {
                    breakpoint_type: BreakpointType::SourceBreakpoint {
                        source: Box::new(source.clone()),
                        location: SourceLocationScope::Specific(source_location),
                    },
                    address,
                });
            }
        }

        let breakpoint_body = SetBreakpointsResponseBody {
            breakpoints: created_breakpoints,
        };
        self.send_response(request, Ok(Some(breakpoint_body)))
    }

    pub(crate) async fn set_instruction_breakpoints(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let arguments: SetInstructionBreakpointsArguments = get_arguments(self, request)?;
        let requested: Vec<InstructionBreakpoint> = arguments.breakpoints;

        // Parse memory references and collect existing instruction bps to clear.
        let (parsed, clear_addrs) = {
            let core_data = match session_data.core_data_mut(core_index) {
                Err(error) => return self.send_response::<()>(request, Err(&error)),
                Ok(core_data) => core_data,
            };
            let clear_addrs: Vec<u64> = core_data
                .breakpoints
                .iter()
                .filter(|ab| matches!(ab.breakpoint_type, BreakpointType::InstructionBreakpoint))
                .map(|ab| ab.address)
                .collect();
            core_data
                .breakpoints
                .retain(|ab| !matches!(ab.breakpoint_type, BreakpointType::InstructionBreakpoint));
            let parsed: Vec<Option<u64>> = requested
                .iter()
                .map(|rb| {
                    MemoryAddress::try_from(rb.instruction_reference.as_str())
                        .ok()
                        .map(|MemoryAddress(addr)| addr)
                })
                .collect();
            (parsed, clear_addrs)
        };

        if let Err(error) = session_data
            .backend
            .clear_hw_breakpoints(core_index, clear_addrs)
            .await
        {
            tracing::warn!("Failed to clear instruction breakpoints. {}", error);
        }
        let set_addrs: Vec<u64> = parsed.iter().copied().flatten().collect();
        let set_results = session_data
            .backend
            .set_hw_breakpoints(core_index, set_addrs.clone())
            .await
            .map_err(|e| {
                DebuggerError::Other(anyhow!("Failed to set instruction breakpoints: {e}"))
            })?;

        let source_locations = session_data
            .backend
            .resolve_source_locations(set_addrs.clone())
            .await
            .unwrap_or_else(|error| {
                tracing::debug!("Could not resolve instruction breakpoint sources: {error}");
                vec![None; set_addrs.len()]
            });

        let mut breakpoints: Vec<Breakpoint> = Vec::with_capacity(requested.len());
        let mut to_cache: Vec<u64> = Vec::new();
        let mut set_idx = 0;
        for (i, rb) in requested.iter().enumerate() {
            match parsed[i] {
                None => breakpoints.push(Breakpoint {
                    column: None,
                    end_column: None,
                    end_line: None,
                    id: None,
                    instruction_reference: Some(rb.instruction_reference.clone()),
                    line: None,
                    message: Some(format!(
                        "Invalid memory reference specified: {:?}",
                        rb.instruction_reference
                    )),
                    offset: None,
                    source: None,
                    verified: false,
                    reason: None,
                }),
                Some(memory_reference) => {
                    let set_result = set_results.get(set_idx);
                    set_idx += 1;
                    if matches!(set_result, Some(Ok(()))) {
                        to_cache.push(memory_reference);
                    }
                    let source_location = if matches!(set_result, Some(Ok(()))) {
                        source_locations.get(set_idx - 1).cloned().flatten()
                    } else {
                        None
                    };
                    let set_error =
                        set_result.and_then(|r| r.as_ref().err().map(|e| e.to_string()));
                    breakpoints.push(instruction_breakpoint_response(
                        memory_reference,
                        matches!(set_result, Some(Ok(()))),
                        set_error.as_deref(),
                        source_location.as_ref(),
                    ));
                }
            }
        }

        if let Ok(core_data) = session_data.core_data_mut(core_index) {
            for address in to_cache {
                core_data.breakpoints.push(ActiveBreakpoint {
                    breakpoint_type: BreakpointType::InstructionBreakpoint,
                    address,
                });
            }
        }

        for breakpoint_response in &breakpoints {
            if !breakpoint_response.verified
                && let Some(message) = &breakpoint_response.message
            {
                self.log_to_console(format!("Warning: {message}"));
                self.show_message(MessageSeverity::Warning, message.clone());
            }
        }

        self.send_response(
            request,
            Ok(Some(SetInstructionBreakpointsResponseBody { breakpoints })),
        )
    }

    pub(crate) async fn threads(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        if !self.configuration_is_done() {
            let current_core_status = session_data.backend.status(core_index).await?;

            return
                self.send_response::<()>(
                    request,
                    Err(&DebuggerError::Other(anyhow!(
                        "Received request for `threads`, while last known core status was {current_core_status:?}",
                    ))),
                );
        }

        let threads = vec![Thread {
            id: core_index as i64,
            name: session_data
                .core_data_opt(core_index)
                .map(|cd| cd.target_name.clone())
                .unwrap_or_default(),
        }];
        self.send_response(request, Ok(Some(ThreadsResponseBody { threads })))
    }

    pub(crate) async fn stack_trace(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let status = session_data
            .backend
            .status(core_index)
            .await
            .map_err(DebuggerError::ProbeRs)?;
        if !status.is_halted() {
            return self.send_response::<()>(
                request,
                Err(&DebuggerError::Other(anyhow!(
                    "Core must be halted before requesting a stack trace"
                ))),
            );
        }

        let arguments: StackTraceArguments = get_arguments(self, request)?;

        let core_data = match session_data.core_data(core_index) {
            Ok(core_data) => core_data,
            Err(_) => {
                return self.send_response::<()>(
                    request,
                    Err(&DebuggerError::Other(anyhow!(
                        "No core data for core index {core_index}"
                    ))),
                );
            }
        };

        let total_frames = core_data.stack_frames.len() as i64;

        let mut levels = arguments.levels.unwrap_or(0);
        let start_frame = arguments.start_frame.unwrap_or(0);

        tracing::debug!("Start frame: {} Levels: {}", start_frame, levels);

        if levels == 0 {
            levels = total_frames;
        }

        const PAGE_SIZE: i64 = 50;

        let first_frame = start_frame as usize;
        let last_frame = if total_frames < PAGE_SIZE {
            total_frames
        } else {
            start_frame + levels
        } as usize;

        let Some(frames) = core_data.stack_frames.get(first_frame..last_frame) else {
            return self.send_response::<()>(
                request,
                Err(&DebuggerError::Other(anyhow!(
                    "Request for stack trace failed with invalid arguments: {arguments:?}"
                ))),
            );
        };

        let frame_list: Vec<StackFrame> = frames
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

                let source = if let Some(source_location) = &frame.source_location {
                    get_dap_source(source_location)
                } else {
                    tracing::debug!("No source location present for frame!");
                    None
                };

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
    pub(crate) async fn disassemble(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let arguments: DisassembleArguments = get_arguments(self, request)?;

        if let Ok(memory_reference) = parse_int::parse::<u64>(&arguments.memory_reference) {
            match session_data
                .backend
                .disassemble(
                    core_index,
                    memory_reference,
                    arguments.offset.unwrap_or(0_i64),
                    arguments.instruction_offset.unwrap_or(0_i64),
                    arguments.instruction_count,
                )
                .await
            {
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

    pub(crate) async fn r#continue(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        match self
            .continue_impl_async(
                &mut session_data.backend,
                session_data
                    .core_data
                    .iter_mut()
                    .find(|c| c.core_index == core_index)
                    .ok_or_else(|| {
                        DebuggerError::Other(anyhow!("No core data for core {core_index}"))
                    })?,
            )
            .await
        {
            Ok(all_continued) if request.command == "continue" => {
                // If this continue was initiated as part of some other request, then do not respond.
                self.send_response(
                    request,
                    Ok(Some(ContinueResponseBody {
                        all_threads_continued: Some(all_continued),
                    })),
                )
            }
            Ok(_) => Ok(()),
            Err(error) => {
                self.send_response::<()>(request, Err(&DebuggerError::Other(anyhow!("{error}"))))?;
                Err(error)
            }
        }
    }

    /// Steps through the code at the requested granularity.
    /// - [SteppingMode::StepInstruction]: If MS DAP [SteppingGranularity::Instruction] (usually sent from the disassembly view)
    /// - [SteppingMode::OverStatement]: In all other cases.
    pub(crate) async fn next(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let arguments: NextArguments = get_arguments(self, request)?;

        let stepping_granularity = match arguments.granularity {
            Some(SteppingGranularity::Instruction) => SteppingMode::StepInstruction,
            _ => SteppingMode::OverStatement,
        };

        self.debug_step(stepping_granularity, session_data, core_index, request)
            .await
    }

    /// Steps through the code at the requested granularity.
    /// - [SteppingMode::StepInstruction]: If MS DAP [SteppingGranularity::Instruction] (usually sent from the disassembly view)
    /// - [SteppingMode::IntoStatement]: In all other cases.
    pub(crate) async fn step_in(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let arguments: StepInArguments = get_arguments(self, request)?;

        let stepping_granularity = match arguments.granularity {
            Some(SteppingGranularity::Instruction) => SteppingMode::StepInstruction,
            _ => SteppingMode::IntoStatement,
        };
        self.debug_step(stepping_granularity, session_data, core_index, request)
            .await
    }

    /// Steps through the code at the requested granularity.
    /// - [SteppingMode::StepInstruction]: If MS DAP [SteppingGranularity::Instruction] (usually sent from the disassembly view)
    /// - [SteppingMode::OutOfStatement]: In all other cases.
    pub(crate) async fn step_out(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<()> {
        let arguments: StepOutArguments = get_arguments(self, request)?;

        let stepping_granularity = match arguments.granularity {
            Some(SteppingGranularity::Instruction) => SteppingMode::StepInstruction,
            _ => SteppingMode::OutOfStatement,
        };

        self.debug_step(stepping_granularity, session_data, core_index, request)
            .await
    }

    /// Common code for the `next`, `step_in`, and `step_out` methods.
    async fn debug_step(
        &mut self,
        stepping_mode: SteppingMode,
        session_data: &mut SessionData,
        core_index: usize,
        request: &Request,
    ) -> Result<(), anyhow::Error> {
        self.step_impl_async(
            stepping_mode,
            &mut session_data.backend,
            session_data
                .core_data
                .iter_mut()
                .find(|c| c.core_index == core_index)
                .ok_or_else(|| anyhow!("No core data for core {core_index}"))?,
        )
        .await?;
        self.send_response::<()>(request, Ok(None))?;

        Ok(())
    }

    /// Session-level `restart`: reset-and-halt (reapplying breakpoints on
    /// Riscv/Xtensa via `backend.core_architecture`, which is cached), then
    /// optionally resume via `continue_impl_async`, emitting the
    /// `stopped`/`continued` DAP events. The per-core `reset_and_halt_core`
    /// remains for the REPL path until cluster 6.8.
    pub(crate) async fn restart_async(
        &mut self,
        session_data: &mut SessionData,
        core_index: usize,
        request: Option<&Request>,
    ) -> Result<()> {
        let core_info = self
            .reset_and_halt_core_async(
                &mut session_data.backend,
                session_data
                    .core_data
                    .iter_mut()
                    .find(|c| c.core_index == core_index)
                    .ok_or_else(|| {
                        DebuggerError::Other(anyhow!("No core data for core {core_index}"))
                    })?,
            )
            .await?;

        if let Some(request) = request {
            if !self.halt_after_reset {
                self.continue_impl_async(
                    &mut session_data.backend,
                    session_data
                        .core_data
                        .iter_mut()
                        .find(|c| c.core_index == core_index)
                        .ok_or_else(|| {
                            DebuggerError::Other(anyhow!("No core data for core {core_index}"))
                        })?,
                )
                .await?;

                self.send_response::<()>(request, Ok(None))?;
                let event_body = Some(ContinuedEventBody {
                    all_threads_continued: Some(false),
                    thread_id: core_index as i64,
                });
                self.send_event("continued", event_body)?;
            } else {
                self.send_response::<()>(request, Ok(None))?;
                let event_body = Some(StoppedEventBody {
                    reason: "restart".to_owned(),
                    description: Some(
                        CoreStatus::Halted(HaltReason::External)
                            .short_long_status(None)
                            .1,
                    ),
                    thread_id: Some(core_index as i64),
                    preserve_focus_hint: None,
                    text: None,
                    all_threads_stopped: Some(self.all_cores_halted),
                    hit_breakpoint_ids: None,
                });
                self.send_event("stopped", event_body)?;
            }
        } else if self.configuration_is_done() {
            let event_body = Some(StoppedEventBody {
                reason: "restart".to_owned(),
                description: Some(
                    CoreStatus::Halted(HaltReason::External)
                        .short_long_status(Some(core_info.pc))
                        .1,
                ),
                thread_id: Some(core_index as i64),
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

    /// Sends an error response, unless the request already has a response.
    pub fn send_error_response(&mut self, request: &Request, error: &DebuggerError) -> Result<()> {
        if self.adapter.has_pending_request(request.seq) {
            self.send_response::<()>(request, Err(error))
        } else {
            Ok(())
        }
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
        self.adapter.send_event(event_type, event_body)
    }

    pub fn log_to_console(&mut self, message: impl AsRef<str>) -> bool {
        self.adapter.log_to_console(message.as_ref())
    }

    /// Send a custom "probe-rs-show-message" event to the MS DAP Client.
    /// The `severity` field can be one of `information`, `warning`, or `error`.
    pub fn show_message(&mut self, severity: MessageSeverity, message: impl AsRef<str>) -> bool {
        self.adapter.show_message(severity, message)
    }

    /// Send a custom `probe-rs-rtt-channel-config` event to the MS DAP Client, to create a window for a specific RTT channel.
    pub fn rtt_window(
        &mut self,
        channel_number: u32,
        channel_name: String,
        data_format: DataFormat,
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

    /// Send a custom `probe-rs-rtt-channel-config` event to the MS DAP Client, to create a window for a specific RTT channel.
    pub fn open_prompt(&mut self, kind: PromptKind, name: &str, handle: u32) -> bool {
        let Ok(event_body) = serde_json::to_value(CreatePromptEventBody {
            prompt_kind: kind,
            prompt_name: name.to_string(),
            prompt_handle: handle,
        }) else {
            return false;
        };

        self.send_event("probe-rs-create-prompt", Some(event_body))
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

impl DebugAdapter {
    /// Halt the core (REPL/DAP `pause`).
    pub(crate) async fn pause_impl_async(
        &mut self,
        backend: &mut RpcBackend,
        core_data: &mut CoreData,
    ) -> Result<CoreInformation> {
        let core_index = core_data.core_index;
        core_data.invalidate_stack_frame_cache();
        let cpu_info = backend.halt(core_index, Duration::from_millis(500)).await?;
        let new_status = CoreStatus::Halted(HaltReason::Request);
        let event_body = Some(StoppedEventBody {
            reason: "pause".to_owned(),
            description: Some(new_status.short_long_status(Some(cpu_info.pc)).1),
            thread_id: Some(core_index as i64),
            preserve_focus_hint: Some(false),
            text: None,
            all_threads_stopped: Some(self.all_cores_halted),
            hit_breakpoint_ids: None,
        });
        core_data.last_known_status = new_status;
        self.dyn_send_event(
            "stopped",
            event_body.map(|event_body| serde_json::to_value(event_body).unwrap_or_default()),
        )?;
        Ok(cpu_info)
    }

    /// Resume the core (REPL `c` / DAP `continue`).
    pub(crate) async fn continue_impl_async(
        &mut self,
        backend: &mut RpcBackend,
        core_data: &mut CoreData,
    ) -> Result<bool> {
        core_data.invalidate_stack_frame_cache();
        backend.run(core_data.core_index).await?;
        core_data.last_known_status = CoreStatus::Unknown;
        self.all_cores_halted = false;
        Ok(true) // TODO this isn't very useful?
    }

    /// Reset and halt the core (REPL `reset` / DAP `restart`), re-applying
    /// hardware breakpoints on RISC-V / Xtensa.
    pub(crate) async fn reset_and_halt_core_async(
        &mut self,
        backend: &mut RpcBackend,
        core_data: &mut CoreData,
    ) -> Result<CoreInformation> {
        let core_index = core_data.core_index;
        core_data.invalidate_stack_frame_cache();
        let core_info: CoreInformation = backend
            .reset_and_halt(core_index, Duration::from_millis(500))
            .await
            .map_err(DebuggerError::ProbeRs)?;

        let arch = backend
            .core_metadata
            .get(core_index)
            .map(|m| m.architecture)
            .ok_or_else(|| {
                probe_rs::Error::Other(format!("No core metadata for core {core_index}"))
            })
            .map_err(DebuggerError::ProbeRs)?;
        if [Architecture::Riscv, Architecture::Xtensa].contains(&arch) {
            let addrs: Vec<u64> = core_data.breakpoints.iter().map(|bp| bp.address).collect();
            if !addrs.is_empty() {
                backend
                    .set_hw_breakpoints(core_index, addrs)
                    .await
                    .map_err(|e| {
                        DebuggerError::Other(anyhow!(
                            "Failed to re-apply breakpoints after reset: {e}"
                        ))
                    })?;
            }
        }

        core_data.last_known_status = CoreStatus::Unknown;
        self.all_cores_halted = false;
        Ok(core_info)
    }

    pub(crate) async fn step_impl_async(
        &mut self,
        stepping_mode: SteppingMode,
        backend: &mut RpcBackend,
        core_data: &mut CoreData,
    ) -> Result<u64> {
        let core_index = core_data.core_index;
        // Mark the adapter's cached status unknown before the server performs
        // the step, without notifying the client.
        core_data.invalidate_stack_frame_cache();
        core_data.last_known_status = CoreStatus::Unknown;
        self.all_cores_halted = false;

        let (new_status, program_counter, warning) = backend
            .debug_step(core_index, stepping_mode)
            .await
            .map_err(DebuggerError::ProbeRs)?;
        if let Some(message) = warning {
            self.dyn_show_message(
                MessageSeverity::Information,
                format!("Step error @{program_counter:#010X}: {message}"),
            );
        }

        // Override the halt reason: stepping uses breakpoints, which would
        // otherwise surface as a "BreakPoint" halt reason.
        core_data.last_known_status = CoreStatus::Halted(HaltReason::Step);
        if matches!(new_status, CoreStatus::Halted(_)) {
            let event_body = StoppedEventBody {
                reason: core_data
                    .last_known_status
                    .short_long_status(None)
                    .0
                    .to_string(),
                description: Some(
                    CoreStatus::Halted(HaltReason::Step)
                        .short_long_status(Some(program_counter))
                        .1,
                ),
                thread_id: Some(core_index as i64),
                preserve_focus_hint: None,
                text: None,
                all_threads_stopped: Some(self.all_cores_halted),
                hit_breakpoint_ids: None,
            };
            self.dyn_send_event("stopped", serde_json::to_value(event_body).ok())?;
        }
        Ok(program_counter)
    }

    #[tracing::instrument(level = "trace", skip_all)]
    pub fn dyn_send_event(&mut self, event_type: &str, event_body: Option<Value>) -> Result<()> {
        self.adapter.dyn_send_event(event_type, event_body)
    }

    /// Send a custom "probe-rs-show-message" event to the MS DAP Client.
    /// The `severity` field can be one of `information`, `warning`, or `error`.
    pub fn dyn_show_message(&mut self, severity: MessageSeverity, message: String) -> bool {
        self.adapter.dyn_show_message(severity, message)
    }
}

fn find_register_by_dap_name(
    registers: &DebugRegisters,
    register_name: &str,
) -> Option<DebugRegister> {
    registers
        .0
        .iter()
        .find(|register| register.get_register_name() == register_name)
        .cloned()
        .or_else(|| registers.get_register_by_name(register_name))
}

fn parse_register_value(value: &str, register: &CoreRegister) -> Result<RegisterValue> {
    let RegisterDataType::UnsignedInteger(size_in_bits) = register.data_type() else {
        return Err(anyhow!(
            "Writing non-integer register {} is not supported.",
            register
        ));
    };

    anyhow::ensure!(
        (1..=128).contains(&size_in_bits),
        "Register {} has unsupported width {size_in_bits} bits.",
        register
    );

    let parsed_value = parse_unsigned_register_value(value)?;
    let max_value = if size_in_bits == 128 {
        u128::MAX
    } else {
        (1_u128 << size_in_bits) - 1
    };

    anyhow::ensure!(
        parsed_value <= max_value,
        "Value {value:?} does not fit in {}-bit register {}.",
        size_in_bits,
        register
    );

    Ok(match size_in_bits {
        1..=32 => RegisterValue::U32(parsed_value as u32),
        33..=64 => RegisterValue::U64(parsed_value as u64),
        65..=128 => RegisterValue::U128(parsed_value),
        _ => unreachable!("register width was checked above"),
    })
}

fn parse_unsigned_register_value(value: &str) -> Result<u128> {
    let trimmed_value = value.trim();
    anyhow::ensure!(!trimmed_value.is_empty(), "Register value cannot be empty.");
    anyhow::ensure!(
        !trimmed_value.starts_with('-'),
        "Register value must be unsigned."
    );

    let trimmed_value = trimmed_value.strip_prefix('+').unwrap_or(trimmed_value);
    let normalized_value = trimmed_value.replace('_', "");
    anyhow::ensure!(
        !normalized_value.is_empty(),
        "Register value cannot be empty."
    );

    let (digits, radix) = normalized_value
        .strip_prefix("0x")
        .or_else(|| normalized_value.strip_prefix("0X"))
        .map(|digits| (digits, 16))
        .unwrap_or((normalized_value.as_str(), 10));

    anyhow::ensure!(!digits.is_empty(), "Register value cannot be empty.");

    u128::from_str_radix(digits, radix).with_context(|| format!("Invalid register value {value:?}"))
}

fn register_requires_exact_readback(register: &CoreRegister) -> bool {
    !register.register_has_role(RegisterRole::ProgramCounter)
        && !register.register_has_role(RegisterRole::ProcessorStatus)
        && !register.register_has_role(RegisterRole::FloatingPointStatus)
}

fn register_write_requires_stack_frame_refresh(register: &CoreRegister) -> bool {
    register.unwind_rule != UnwindRule::Clear
        || register.register_has_role(RegisterRole::ProgramCounter)
        || register.register_has_role(RegisterRole::FramePointer)
        || register.register_has_role(RegisterRole::StackPointer)
        || register.register_has_role(RegisterRole::MainStackPointer)
        || register.register_has_role(RegisterRole::ProcessStackPointer)
        || register.register_has_role(RegisterRole::ReturnAddress)
}

pub fn get_arguments<T: DeserializeOwned>(
    debug_adapter: &mut DebugAdapter,
    req: &Request,
) -> Result<T, DebuggerError> {
    let Some(raw_arguments) = &req.arguments else {
        debug_adapter.send_response::<()>(req, Err(&DebuggerError::InvalidRequest))?;
        return Err(DebuggerError::Other(anyhow!(
            "Failed to get {} arguments",
            req.command
        )));
    };

    match T::deserialize(raw_arguments) {
        Ok(value) => Ok(value),
        Err(e) => {
            let err = anyhow!(
                "Failed to deserialize {} arguments: {}, error: {}",
                req.command,
                hide_file_payloads(raw_arguments),
                e
            );

            debug_adapter.send_response::<()>(req, Err(&e.into()))?;
            Err(DebuggerError::Other(err))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use probe_rs::{RegisterId, UnwindRule};

    static U32_ROLES: [RegisterRole; 1] = [RegisterRole::Core("r0")];
    static U32_REGISTER: CoreRegister = CoreRegister {
        id: RegisterId(0),
        roles: &U32_ROLES,
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    };

    static U64_ROLES: [RegisterRole; 1] = [RegisterRole::Core("x0")];
    static U64_REGISTER: CoreRegister = CoreRegister {
        id: RegisterId(1),
        roles: &U64_ROLES,
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    };

    static U128_ROLES: [RegisterRole; 1] = [RegisterRole::Core("v0")];
    static U128_REGISTER: CoreRegister = CoreRegister {
        id: RegisterId(2),
        roles: &U128_ROLES,
        data_type: RegisterDataType::UnsignedInteger(128),
        unwind_rule: UnwindRule::Clear,
    };

    static FP_ROLES: [RegisterRole; 1] = [RegisterRole::Core("f0")];
    static FP_REGISTER: CoreRegister = CoreRegister {
        id: RegisterId(3),
        roles: &FP_ROLES,
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    };

    static PC_ROLES: [RegisterRole; 2] = [RegisterRole::Core("pc"), RegisterRole::ProgramCounter];
    static PC_REGISTER: CoreRegister = CoreRegister {
        id: RegisterId(4),
        roles: &PC_ROLES,
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    };

    static A0_ROLES: [RegisterRole; 2] = [RegisterRole::Core("a0"), RegisterRole::Other("x10")];
    static A0_REGISTER: CoreRegister = CoreRegister {
        id: RegisterId(10),
        roles: &A0_ROLES,
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    };

    static STATUS_ROLES: [RegisterRole; 2] =
        [RegisterRole::Core("xpsr"), RegisterRole::ProcessorStatus];
    static STATUS_REGISTER: CoreRegister = CoreRegister {
        id: RegisterId(16),
        roles: &STATUS_ROLES,
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    };

    static SP_ROLES: [RegisterRole; 2] = [RegisterRole::Core("sp"), RegisterRole::StackPointer];
    static SP_REGISTER: CoreRegister = CoreRegister {
        id: RegisterId(17),
        roles: &SP_ROLES,
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    };

    static LR_ROLES: [RegisterRole; 2] = [RegisterRole::Core("lr"), RegisterRole::ReturnAddress];
    static LR_REGISTER: CoreRegister = CoreRegister {
        id: RegisterId(18),
        roles: &LR_ROLES,
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    };

    static PRESERVED_ROLES: [RegisterRole; 1] = [RegisterRole::Core("r4")];
    static PRESERVED_REGISTER: CoreRegister = CoreRegister {
        id: RegisterId(19),
        roles: &PRESERVED_ROLES,
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    };

    #[test]
    fn evaluate_dispatch_rejects_unsupported_contexts() {
        assert_eq!(
            evaluate_dispatch(Some("clipboard")),
            EvaluateDispatch::Unsupported
        );
        assert_eq!(
            evaluate_dispatch(Some("variables")),
            EvaluateDispatch::Unsupported
        );
        assert_eq!(evaluate_dispatch(None), EvaluateDispatch::Unsupported);
    }

    #[test]
    fn parse_register_value_supports_decimal_hex_and_separators() -> Result<()> {
        assert_eq!(
            parse_register_value("4660", &U32_REGISTER)?,
            RegisterValue::U32(0x1234)
        );
        assert_eq!(
            parse_register_value("0x1234", &U32_REGISTER)?,
            RegisterValue::U32(0x1234)
        );
        assert_eq!(
            parse_register_value("0X1234", &U32_REGISTER)?,
            RegisterValue::U32(0x1234)
        );
        assert_eq!(
            parse_register_value("0x1_0000", &U32_REGISTER)?,
            RegisterValue::U32(0x1_0000)
        );

        Ok(())
    }

    #[test]
    fn parse_register_value_selects_storage_width() -> Result<()> {
        assert_eq!(
            parse_register_value("0xffff_ffff", &U32_REGISTER)?,
            RegisterValue::U32(u32::MAX)
        );
        assert_eq!(
            parse_register_value("0xffff_ffff_ffff_ffff", &U64_REGISTER)?,
            RegisterValue::U64(u64::MAX)
        );
        assert_eq!(
            parse_register_value("0xffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff", &U128_REGISTER)?,
            RegisterValue::U128(u128::MAX)
        );

        Ok(())
    }

    #[test]
    fn parse_register_value_rejects_invalid_input() {
        assert!(parse_register_value("", &U32_REGISTER).is_err());
        assert!(parse_register_value("-1", &U32_REGISTER).is_err());
        assert!(parse_register_value("0xnot_hex", &U32_REGISTER).is_err());
        assert!(parse_register_value("0x1_0000_0000", &U32_REGISTER).is_err());
        assert!(parse_register_value("1", &FP_REGISTER).is_err());
    }

    #[test]
    fn find_register_by_dap_name_matches_display_name_first() -> Result<()> {
        let registers = DebugRegisters(vec![
            DebugRegister {
                core_register: &PC_REGISTER,
                dwarf_id: Some(0),
                value: Some(RegisterValue::U32(0x1000)),
            },
            DebugRegister {
                core_register: &A0_REGISTER,
                dwarf_id: Some(10),
                value: Some(RegisterValue::U32(1)),
            },
        ]);

        let pc_register = find_register_by_dap_name(&registers, "pc/PC")
            .ok_or_else(|| anyhow::anyhow!("pc/PC register not found"))?;
        assert_eq!(pc_register.core_register.id, PC_REGISTER.id);
        let a0_register = find_register_by_dap_name(&registers, "a0/x10")
            .ok_or_else(|| anyhow::anyhow!("a0/x10 register not found"))?;
        assert_eq!(a0_register.core_register.id, A0_REGISTER.id);

        Ok(())
    }

    #[test]
    fn find_register_by_dap_name_falls_back_to_role_names() -> Result<()> {
        let registers = DebugRegisters(vec![DebugRegister {
            core_register: &A0_REGISTER,
            dwarf_id: Some(10),
            value: Some(RegisterValue::U32(1)),
        }]);

        let a0_register = find_register_by_dap_name(&registers, "x10")
            .ok_or_else(|| anyhow::anyhow!("x10 register not found"))?;
        assert_eq!(a0_register.core_register.id, A0_REGISTER.id);
        assert!(find_register_by_dap_name(&registers, "x11").is_none());

        Ok(())
    }

    #[test]
    fn exact_readback_is_not_required_for_hardware_normalized_registers() {
        assert!(register_requires_exact_readback(&U32_REGISTER));
        assert!(register_requires_exact_readback(&A0_REGISTER));
        assert!(!register_requires_exact_readback(&PC_REGISTER));
        assert!(!register_requires_exact_readback(&STATUS_REGISTER));
    }

    #[test]
    fn stack_frame_refresh_is_required_for_unwind_sensitive_registers() {
        assert!(!register_write_requires_stack_frame_refresh(&U32_REGISTER));
        assert!(register_write_requires_stack_frame_refresh(&PC_REGISTER));
        assert!(register_write_requires_stack_frame_refresh(&SP_REGISTER));
        assert!(register_write_requires_stack_frame_refresh(&LR_REGISTER));
        assert!(register_write_requires_stack_frame_refresh(
            &PRESERVED_REGISTER
        ));
    }
}
