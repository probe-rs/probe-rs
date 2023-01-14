use crate::{
    debug_adapter::{dap_types, protocol::ProtocolAdapter},
    debugger::{
        configuration::ConsoleLog,
        core_data::CoreHandle,
        session_data::{ActiveBreakpoint, BreakpointType},
    },
    DebuggerError,
};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose as base64_engine, Engine as _};
use capstone::{
    arch::arm::ArchMode as armArchMode, arch::arm64::ArchMode as aarch64ArchMode,
    arch::riscv::ArchMode as riscvArchMode, prelude::*, Capstone, Endian,
};
use dap_types::*;
use parse_int::parse;
use probe_rs::{
    architecture::{arm::ArmError, riscv::communication_interface::RiscvError},
    debug::{
        ColumnType, DebugRegisters, SourceLocation, SteppingMode, VariableName, VariableNodeType,
    },
    Architecture::Riscv,
    CoreStatus, CoreType, Error, HaltReason, InstructionSet, MemoryInterface, RegisterValue,
};
use probe_rs_cli_util::rtt;
use serde::{de::DeserializeOwned, Serialize};
use std::{convert::TryInto, path::Path, str, string::ToString, time::Duration};

/// Progress ID used for progress reporting when the debug adapter protocol is used.
type ProgressId = i64;

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
    /// Flag to indicate if all cores of the target are halted. This is used to accurately report the `all_threads_stopped` field in the DAP `StoppedEvent`, as well as to prevent unnecessary polling of core status.
    /// The default is `true`, and will be set to `false` if any of the cores report a status other than `CoreStatus::Halted(_)`.
    pub(crate) all_cores_halted: bool,
    /// Progress ID used for progress reporting when the debug adapter protocol is used.
    progress_id: ProgressId,
    /// Flag to indicate if the connected client supports progress reporting.
    pub(crate) supports_progress_reporting: bool,
    /// Flags to improve breakpoint accuracy.
    /// DWARF spec at Sect 2.14 uses 1 based numbering, with a 0 indicating not-specified. We will follow that standard, and translate incoming requests depending on the DAP Client treatment of 0 or 1 based numbering.
    pub(crate) lines_start_at_1: bool,
    /// DWARF spec at Sect 2.14 uses 1 based numbering, with a 0 indicating not-specified. We will follow that standard, and translate incoming requests depending on the DAP Client treatment of 0 or 1 based numbering.
    pub(crate) columns_start_at_1: bool,
    adapter: P,
}

impl<P: ProtocolAdapter> DebugAdapter<P> {
    pub fn new(adapter: P) -> DebugAdapter<P> {
        DebugAdapter {
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

    pub(crate) fn pause(&mut self, target_core: &mut CoreHandle, request: Request) -> Result<()> {
        match target_core.core.halt(Duration::from_millis(500)) {
            Ok(cpu_info) => {
                let new_status = match target_core.core.status() {
                    Ok(new_status) => new_status,
                    Err(error) => {
                        self.send_response::<()>(request, Err(DebuggerError::ProbeRs(error)))?;
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
                self.send_event("stopped", event_body)?;
                Ok(())
            }
            Err(error) => {
                self.send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error))))
            }
        }
    }

    pub(crate) fn read_memory(
        &mut self,
        target_core: &mut CoreHandle,
        request: Request,
    ) -> Result<()> {
        let arguments: ReadMemoryArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };
        let memory_offset = arguments.offset.unwrap_or(0);
        let mut address: u64 =
            if let Ok(address) = parse::<u64>(arguments.memory_reference.as_ref()) {
                address + memory_offset as u64
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
            if let Ok(good_byte) = target_core.core.read_word_8(address) {
                buff.push(good_byte);
                address += 1;
                num_bytes_unread -= 1;
            } else {
                break;
            }
        }
        if !buff.is_empty() || num_bytes_unread == 0 {
            let response = base64_engine::STANDARD.encode(&buff);
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
        target_core: &mut CoreHandle,
        request: Request,
    ) -> Result<()> {
        let arguments: WriteMemoryArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };
        let memory_offset = arguments.offset.unwrap_or(0);
        let address: u64 = if let Ok(address) = parse::<i64>(arguments.memory_reference.as_ref()) {
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
        let data_bytes = match base64_engine::STANDARD.decode(&arguments.data) {
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
    pub(crate) fn evaluate(
        &mut self,
        target_core: &mut CoreHandle,
        request: Request,
    ) -> Result<()> {
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

        // The Variables request sometimes returns the variable name, and other times the variable id, so this expression will be tested to determine if it is an id or not.
        let expression = arguments.expression.clone();

        // Make sure we have a valid StackFrame
        if let Some(stack_frame) = match arguments.frame_id {
            Some(frame_id) => target_core
                .core_data
                .stack_frames
                .iter_mut()
                .find(|stack_frame| stack_frame.id == frame_id),
            None => {
                // Use the current frame_id
                target_core.core_data.stack_frames.first_mut()
            }
        } {
            // Always search the registers first, because we don't have a VariableCache for them.
            if let Some(register_value) = stack_frame
                .registers
                .get_register_by_name(expression.as_str())
                .and_then(|reg| reg.value)
            {
                response_body.type_ = Some(format!("{}", VariableName::RegistersRoot));
                response_body.result = format!("{}", register_value);
            } else {
                // If the expression wasn't pointing to a register, then check if is a local or static variable in our stack_frame
                let mut variable: Option<probe_rs::debug::Variable> = None;
                let mut variable_cache: Option<&mut probe_rs::debug::VariableCache> = None;
                // Search through available caches and stop as soon as the variable is found
                #[allow(clippy::manual_flatten)]
                for variable_cache_entry in [
                    stack_frame.local_variables.as_mut(),
                    stack_frame.static_variables.as_mut(),
                    target_core
                        .core_data
                        .core_peripherals
                        .as_mut()
                        .map(|core_peripherals| &mut core_peripherals.svd_variable_cache),
                ] {
                    if let Some(search_cache) = variable_cache_entry {
                        if let Ok(expression_as_key) = expression.parse::<i64>() {
                            variable = search_cache.get_variable_by_key(expression_as_key);
                        } else {
                            variable = search_cache
                                .get_variable_by_name(&VariableName::Named(expression.clone()));
                        }
                        if let Some(variable) = &mut variable {
                            if variable.variable_node_type == VariableNodeType::SvdRegister
                                || variable.variable_node_type == VariableNodeType::SvdField
                            {
                                variable.extract_value(&mut target_core.core, search_cache)
                            }
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
                    response_body.memory_reference = Some(format!("{}", variable.memory_location));
                    response_body.named_variables = Some(named_child_variables_cnt);
                    response_body.result = variable.get_value(variable_cache);
                    response_body.type_ = Some(format!("{:?}", variable.type_name));
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
        target_core: &mut CoreHandle,
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
                let mut variable_cache: Option<&mut probe_rs::debug::VariableCache> = None;
                for search_frame in target_core.core_data.stack_frames.iter_mut() {
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
                        &mut target_core.core,
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
                            response_body.type_ = Some(format!("{:?}", cache_variable.type_name));
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

    pub(crate) fn restart(
        &mut self,
        target_core: &mut CoreHandle,
        request: Option<Request>,
    ) -> Result<()> {
        match target_core.core.halt(Duration::from_millis(500)) {
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

        target_core.reset_core_status(self);
        // Different code paths if we invoke this from a request, versus an internal function.
        if let Some(request) = request {
            // Use reset_and_halt(), and then resume again afterwards, depending on the reset_after_halt flag.
            match target_core.core.reset_and_halt(Duration::from_millis(500)) {
                Ok(_) => {
                    // Ensure ebreak enters debug mode, this is necessary for soft breakpoints to work on architectures like RISC-V.
                    target_core.core.debug_on_sw_breakpoint(true)?;

                    // For RISC-V, we need to re-enable any breakpoints that were previously set, because the core reset 'forgets' them.
                    if target_core.core.architecture() == Riscv {
                        let saved_breakpoints = target_core
                            .core_data
                            .breakpoints
                            .drain(..)
                            .collect::<Vec<ActiveBreakpoint>>();
                        for breakpoint in saved_breakpoints {
                            match target_core.set_breakpoint(
                                breakpoint.breakpoint_address,
                                breakpoint.breakpoint_type.clone(),
                            ) {
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
                        match self.r#continue(target_core, request.clone()) {
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
                                Err(DebuggerError::Other(anyhow!("{}", error))),
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
                }
                Err(error) => self
                    .send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error)))),
            }
        } else {
            // The DAP Client will always do a `reset_and_halt`, and then will consider `halt_after_reset` value after the `configuration_done` request.
            // Otherwise the probe will run past the `main()` before the DAP Client has had a chance to set breakpoints in `main()`.
            match target_core.core.reset_and_halt(Duration::from_millis(500)) {
                Ok(core_info) => {
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
                Err(error) => {
                    if let Some(request) = request {
                        self.send_response::<()>(
                            request,
                            Err(DebuggerError::Other(anyhow!("{}", error))),
                        )
                    } else {
                        self.send_error_response(&DebuggerError::Other(anyhow!("{}", error)))
                    }
                }
            }
        }
    }

    pub(crate) fn configuration_done(
        &mut self,
        _core_data: &mut CoreHandle,
        request: Request,
    ) -> Result<()> {
        self.send_response::<()>(request, Ok(None))
    }

    pub(crate) fn set_breakpoints(
        &mut self,
        target_core: &mut CoreHandle,
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

        // Always clear existing breakpoints for the specified `[crate::debug_adapter::dap_types::Source]` before setting new ones.
        // The DAP Specification doesn't make allowances for deleting and setting individual breakpoints for a specific `Source`.
        match target_core.clear_breakpoints(BreakpointType::SourceBreakpoint(args.source.clone())) {
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

                let (verified_breakpoint, breakpoint_source_location, reason_msg) = if let Some(
                    source_path,
                ) =
                    source_path
                {
                    match target_core.core_data.debug_info.get_breakpoint_location(
                        source_path,
                        requested_breakpoint_line,
                        requested_breakpoint_column,
                    ) {
                        Ok((Some(valid_breakpoint_location), breakpoint_source_location)) => {
                                match target_core.set_breakpoint(
                                    valid_breakpoint_location,
                                    BreakpointType::SourceBreakpoint(args.source.clone()),
                                ) {
                                    Ok(_) => (
                                        Some(valid_breakpoint_location),
                                        breakpoint_source_location,
                                        format!(
                                            "Source breakpoint at memory address: {:#010X}",
                                            valid_breakpoint_location
                                        ),
                                    ),
                                    Err(err) => {
                                        (None, None, format!("Warning: Could not set breakpoint at memory address: {:#010x}: {}", valid_breakpoint_location, err))
                                    }
                                }
                            }
                        Ok(_) => {
                            (None, None, "Cannot set breakpoint here. Try reducing `opt-level` in `Cargo.toml`, or choose a different source location".to_string())
                        }
                        Err(error) => (None, None, format!("Cannot set breakpoint here. Try reducing `opt-level` in `Cargo.toml`, or choose a different source location: {:?}", error)),
                    }
                } else {
                    (None, None, "No source path provided for set_breakpoints(). Please report this as a bug.".to_string())
                };

                if let Some(verified_breakpoint) = verified_breakpoint {
                    created_breakpoints.push(Breakpoint {
                        column: breakpoint_source_location.as_ref().and_then(|sl| {
                            sl.column.map(|col| match col {
                                ColumnType::LeftEdge => 0_i64,
                                ColumnType::Column(c) => c as i64,
                            })
                        }),
                        end_column: None,
                        end_line: None,
                        id: None,
                        line: breakpoint_source_location
                            .and_then(|sl| sl.line.map(|line| line as i64)),
                        message: Some(reason_msg),
                        source: None,
                        instruction_reference: Some(format!("{:#010X}", verified_breakpoint)),
                        offset: None,
                        verified: true,
                    });
                } else {
                    // In addition to sending the error to the 'Hover' message, also write it to the Debug Console Log.
                    self.log_to_console(format!("WARNING: {}", reason_msg));
                    self.show_message(MessageSeverity::Warning, reason_msg.clone());
                    created_breakpoints.push(Breakpoint {
                        column: bp.column,
                        end_column: None,
                        end_line: None,
                        id: None,
                        line: Some(bp.line),
                        message: Some(reason_msg),
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

    pub(crate) fn set_instruction_breakpoints(
        &mut self,
        target_core: &mut CoreHandle,
        request: Request,
    ) -> Result<()> {
        let arguments: SetInstructionBreakpointsArguments = match get_arguments(&request) {
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

        // For returning in the Response
        let mut created_breakpoints: Vec<Breakpoint> = Vec::new();

        // Always clear existing breakpoints before setting new ones.
        match target_core.clear_breakpoints(BreakpointType::InstructionBreakpoint) {
            Ok(_) => {}
            Err(error) => tracing::warn!("Failed to clear instruction breakpoints. {}", error),
        }

        // Set the new (potentially an empty list) breakpoints.
        for requested_breakpoint in arguments.breakpoints {
            let mut breakpoint_response = Breakpoint {
                column: None,
                end_column: None,
                end_line: None,
                id: None,
                instruction_reference: None,
                line: None,
                message: None,
                offset: None,
                source: None,
                verified: false,
            };

            if let Ok(memory_reference) =
                if requested_breakpoint.instruction_reference.starts_with("0x")
                    || requested_breakpoint.instruction_reference.starts_with("0X")
                {
                    u64::from_str_radix(&requested_breakpoint.instruction_reference[2..], 16)
                } else {
                    requested_breakpoint.instruction_reference.parse()
                }
            {
                match target_core
                    .set_breakpoint(memory_reference, BreakpointType::InstructionBreakpoint)
                {
                    Ok(_) => {
                        breakpoint_response.verified = true;
                        breakpoint_response.instruction_reference =
                            Some(format!("{:#010x}", memory_reference));
                        // Try to resolve the source location for this breakpoint.
                        match target_core
                            .core_data
                            .debug_info
                            .get_source_location(memory_reference)
                        {
                            Some(source_location) => {
                                breakpoint_response.source = get_dap_source(&source_location);
                                breakpoint_response.line =
                                    source_location.line.map(|line| line as i64);
                                breakpoint_response.column =
                                    source_location.column.map(|col| match col {
                                        ColumnType::LeftEdge => 0_i64,
                                        ColumnType::Column(c) => c as i64,
                                    });
                            }
                            None => {
                                tracing::debug!("The request `SetInstructionBreakpoints` could not resolve a source location for memory reference: {:#010}", memory_reference);
                            }
                        }
                    }
                    Err(error) => {
                        let message = format!(
                            "Warning: Could not set breakpoint at memory address: {:#010x}: {}",
                            memory_reference, error
                        )
                        .to_string();
                        // In addition to sending the error to the 'Hover' message, also write it to the Debug Console Log.
                        self.log_to_console(format!("Warning: {}", message));
                        self.show_message(MessageSeverity::Warning, message.clone());
                        breakpoint_response.message = Some(message);
                        breakpoint_response.instruction_reference =
                            Some(format!("{:#010x}", memory_reference));
                    }
                }
            } else {
                breakpoint_response.instruction_reference =
                    Some(requested_breakpoint.instruction_reference.clone());
                breakpoint_response.message = Some(format!(
                    "Invalid memory reference specified: {:?}",
                    requested_breakpoint.instruction_reference
                ));
            };
            created_breakpoints.push(breakpoint_response);
        }

        let instruction_breakpoint_body = SetInstructionBreakpointsResponseBody {
            breakpoints: created_breakpoints,
        };
        self.send_response(request, Ok(Some(instruction_breakpoint_body)))
    }

    pub(crate) fn threads(&mut self, target_core: &mut CoreHandle, request: Request) -> Result<()> {
        // TODO: Implement actual thread resolution. For now, we just use the core id as the thread id.
        let current_core_status = target_core.core.status()?;
        let mut threads: Vec<Thread> = vec![];
        if self.configuration_is_done() {
            // We can handle this request normally.
            if current_core_status.is_halted() {
                let single_thread = Thread {
                    id: target_core.core.id() as i64,
                    name: target_core.core_data.target_name.clone(),
                };
                threads.push(single_thread);
                // We do the actual stack trace here, because VSCode sometimes sends multiple StackTrace requests, which lead to unnecessary unwind processing.
                // By doing it here, we do it once, and serve up the results when we get the StackTrace requests.
                let regs = target_core.core.registers();
                let pc = match target_core.core.read_core_reg(regs.program_counter()) {
                    Ok(pc) => pc,
                    Err(error) => {
                        return self
                            .send_response::<()>(request, Err(DebuggerError::ProbeRs(error)))
                    }
                };
                tracing::debug!(
                    "Updating the stack frame data for core #{}",
                    target_core.core.id()
                );

                target_core.core_data.stack_frames = target_core
                    .core_data
                    .debug_info
                    .unwind(&mut target_core.core, pc)?;
                return self.send_response(request, Ok(Some(ThreadsResponseBody { threads })));
            }
        } else {
            // This is the initial call to `threads` that happens after the `configuration_done` request, and requires special handling. (see [`DebugAdapter.configuration_done`])
            self.configuration_done = true;
            // At startup, we have to make sure the DAP Client and the DAP Server are in sync with the status of the core.
            if current_core_status.is_halted() {
                if self.halt_after_reset
                    || matches!(
                        current_core_status,
                        CoreStatus::Halted(HaltReason::Breakpoint(_))
                    )
                {
                    let program_counter = target_core
                        .core
                        .read_core_reg(target_core.core.registers().program_counter())
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
                    return self.send_event("stopped", event_body);
                } else {
                    let single_thread = Thread {
                        id: target_core.core.id() as i64,
                        name: target_core.core_data.target_name.clone(),
                    };
                    threads.push(single_thread);
                    self.send_response(request.clone(), Ok(Some(ThreadsResponseBody { threads })))?;
                    return self.r#continue(target_core, request);
                }
            }
        }
        self.send_response::<()>(
            request,
            Err(DebuggerError::Other(anyhow!(
                "Received request for `threads`, while last known core status was {:?}",
                current_core_status
            ))),
        )
    }

    pub(crate) fn stack_trace(
        &mut self,
        target_core: &mut CoreHandle,
        request: Request,
    ) -> Result<()> {
        match target_core.core.status() {
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

        if let Some(levels) = arguments.levels {
            if let Some(start_frame) = arguments.start_frame {
                // Determine the correct 'slice' of available [StackFrame]s to serve up ...
                let total_frames = target_core.core_data.stack_frames.len() as i64;

                // We need to copy some parts of StackFrame so that we can re-use it later without references to target_core.
                struct PartialStackFrameData {
                    id: i64,
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
                        Err(DebuggerError::Other(anyhow!(
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
                            format!("{} @{}", frame.function_name, frame.pc)
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
                            id: frame.id,
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
    pub(crate) fn scopes(&mut self, target_core: &mut CoreHandle, request: Request) -> Result<()> {
        let arguments: ScopesArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };

        let mut dap_scopes: Vec<Scope> = vec![];

        if let Some(core_peripherals) = &mut target_core.core_data.core_peripherals {
            if let Some(peripherals_root_variable) = core_peripherals
                .svd_variable_cache
                .get_variable_by_name_and_parent(&VariableName::PeripheralScopeRoot, None)
            {
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
                    variables_reference: peripherals_root_variable.variable_key,
                });
            }
        };

        tracing::trace!("Getting scopes for frame {}", arguments.frame_id,);

        if let Some(stack_frame) = target_core.get_stackframe(arguments.frame_id) {
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
        let target_instruction_set = target_core.core.instruction_set()?;
        let mut cs = match target_instruction_set {
            InstructionSet::Thumb2 => {
                let mut capstone_builder = Capstone::new()
                    .arm()
                    .mode(armArchMode::Thumb)
                    .endian(Endian::Little);
                if matches!(target_core.core.core_type(), CoreType::Armv8m) {
                    capstone_builder = capstone_builder
                        .extra_mode(std::iter::once(capstone::arch::arm::ArchExtraMode::V8));
                }
                capstone_builder.build()
            }
            InstructionSet::A32 => Capstone::new()
                .arm()
                .mode(armArchMode::Arm)
                .endian(Endian::Little)
                .build(),
            InstructionSet::A64 => Capstone::new()
                .arm64()
                .mode(aarch64ArchMode::Arm)
                .endian(Endian::Little)
                .build(),
            InstructionSet::RV32 => Capstone::new()
                .riscv()
                .mode(riscvArchMode::RiscV32)
                .endian(Endian::Little)
                .build(),
            InstructionSet::RV32C => Capstone::new()
                .riscv()
                .mode(riscvArchMode::RiscV32)
                .endian(Endian::Little)
                .extra_mode(std::iter::once(
                    capstone::arch::riscv::ArchExtraMode::RiscVC,
                ))
                .build(),
        }
        .map_err(|err| anyhow!("Error creating capstone: {:?}", err))?;
        let _ = cs.set_skipdata(true);

        // Adjust instruction offset as required for variable length instruction sets.
        let instruction_offset_as_bytes = match target_instruction_set {
            InstructionSet::Thumb2 | InstructionSet::RV32C => {
                // Since we cannot guarantee the size of individual instructions, let's assume we will read the 120% of the requested number of 16-bit instructions.
                (instruction_offset
                    * target_core
                        .core
                        .instruction_set()?
                        .get_minimum_instruction_size() as i64)
                    / 4
                    * 5
            }
            InstructionSet::A32 | InstructionSet::A64 | InstructionSet::RV32 => {
                instruction_offset
                    * target_core
                        .core
                        .instruction_set()?
                        .get_minimum_instruction_size() as i64
            }
        };

        // The vector we will use to return results.
        let mut assembly_lines: Vec<DisassembledInstruction> = vec![];

        // The buffer to hold data we read from our target.
        let mut code_buffer: Vec<u8> = vec![];

        // Control whether we need to read target memory in order to disassemble the next instruction.
        let mut read_more_bytes = true;

        // The memory address for the next read from target memory. We have to manually adjust it to be word aligned, and make sure it doesn't underflow/overflow.
        let mut read_pointer = if byte_offset.is_negative() {
            Some(memory_reference.saturating_sub(byte_offset.abs()) as u64)
        } else {
            Some(memory_reference.saturating_add(byte_offset) as u64)
        };
        // We can't rely on the MSDAP arguments to result in a memory aligned address for us to read from, so we force the read_pointer to be a 32-bit memory_aligned address.
        read_pointer = if instruction_offset_as_bytes.is_negative() {
            read_pointer
                .and_then(|rp| {
                    rp.saturating_sub(instruction_offset_as_bytes.unsigned_abs())
                        .checked_div(4)
                })
                .map(|rp_memory_aligned| rp_memory_aligned * 4)
        } else {
            read_pointer
                .and_then(|rp| {
                    rp.saturating_add(instruction_offset_as_bytes as u64)
                        .checked_div(4)
                })
                .map(|rp_memory_aligned| rp_memory_aligned * 4)
        };

        // The memory address for the next instruction to be disassembled
        let mut instruction_pointer = if let Some(read_pointer) = read_pointer {
            read_pointer
        } else {
            let error_message = format!("Unable to calculate starting address for disassembly request with memory reference:{:#010X}, byte offset:{:#010X}, and instruction offset:{:#010X}.", memory_reference, byte_offset, instruction_offset);
            return Err(DebuggerError::Other(anyhow!(error_message)));
        };

        // We will only include source location data in a resulting instruction, if it is different from the previous one.
        let mut stored_source_location = None;

        // The MS DAP spec requires that we always have to return a fixed number of instructions.
        while assembly_lines.len() < instruction_count as usize {
            if read_more_bytes {
                if let Some(current_read_pointer) = read_pointer {
                    // All supported architectures use maximum 32-bit instructions, and require 32-bit memory aligned reads.
                    match target_core.core.read_word_32(current_read_pointer) {
                        Ok(new_word) => {
                            // Advance the read pointer for next time we need it.
                            read_pointer = if let Some(valid_read_pointer) =
                                current_read_pointer.checked_add(4)
                            {
                                Some(valid_read_pointer)
                            } else {
                                // If this happens, the next loop will generate "invalid instruction" records.
                                read_pointer = None;
                                continue;
                            };
                            // Update the code buffer.
                            for new_byte in new_word.to_le_bytes() {
                                code_buffer.push(new_byte);
                            }
                        }
                        Err(memory_read_error) => {
                            // If we can't read data at a given address, then create a "invalid instruction" record, and keep trying.
                            assembly_lines.push(dap_types::DisassembledInstruction {
                                address: format!("{:#010X}", current_read_pointer),
                                column: None,
                                end_column: None,
                                end_line: None,
                                instruction: format!(
                                    "<instruction address not readable : {:?}>",
                                    memory_read_error
                                ),
                                instruction_bytes: None,
                                line: None,
                                location: None,
                                symbol: None,
                            });
                            read_pointer = Some(current_read_pointer.saturating_add(4));
                            continue;
                        }
                    }
                }
            }

            match cs.disasm_all(&code_buffer, instruction_pointer) {
                Ok(instructions) => {
                    if num_traits::Zero::is_zero(&instructions.len()) {
                        // The capstone library sometimes returns an empty result set, instead of an Err. Catch it here or else we risk an infinte loop looking for a valid instruction.
                        return Err(DebuggerError::Other(anyhow::anyhow!(
                            "Disassembly encountered unsupported instructions at memory reference {:#010x?}",
                            instruction_pointer
                        )));
                    }

                    let mut result_instruction = instructions
                        .iter()
                        .map(|instruction| {
                            // Before processing, update the code buffer appropriately
                            code_buffer = code_buffer.split_at(instruction.len()).1.to_vec();

                            // Variable width instruction sets my not use the full `code_buffer`, so we need to read ahead, to ensure we have enough code in the buffer to disassemble the 'widest' of instructions in the instruction set.
                            read_more_bytes = code_buffer.len() < target_instruction_set.get_maximum_instruction_size() as usize;

                            // Move the instruction_pointer for the next read.
                            instruction_pointer += instruction.len() as u64;

                            // Try to resolve the source location for this instruction.
                            // If we find one, we use it ONLY if it is different from the previous one (stored_source_location).
                            // - This helps to reduce visual noise in the VSCode UX, by not displaying the same line of source code multiple times over.
                            // If we do not find a source location, then just return the raw assembly without file/line/column information.
                            let mut location = None;
                            let mut line = None;
                            let mut column = None;
                            if let Some(current_source_location) = target_core
                                .core_data
                                .debug_info
                                .get_source_location(instruction.address()) {
                                if let Some(previous_source_location) = stored_source_location.clone() {
                                    if current_source_location != previous_source_location {
                                        location = get_dap_source(&current_source_location);
                                        line = current_source_location.line.map(|line| line as i64);
                                        column = current_source_location.column.map(|col| match col {
                                            ColumnType::LeftEdge => 0_i64,
                                            ColumnType::Column(c) => c as i64,
                                        });
                                        stored_source_location = Some(current_source_location);
                                    }
                                } else {
                                        stored_source_location = Some(current_source_location);
                                }
                            } else {
                                // It won't affect the outcome, but log it for completeness.
                                tracing::debug!("The request `Disassemble` could not resolve a source location for memory reference: {:#010}", instruction.address());
                            }

                            // Create the instruction data.
                            dap_types::DisassembledInstruction {
                                address: format!("{:#010X}", instruction.address()),
                                column,
                                end_column: None,
                                end_line: None,
                                instruction: format!(
                                    "{}  {}",
                                    instruction.mnemonic().unwrap_or("<unknown>"),
                                    instruction.op_str().unwrap_or("")
                                ),
                                instruction_bytes: None,
                                line,
                                location,
                                symbol: None,
                            }
                        })
                        .collect::<Vec<DisassembledInstruction>>();
                    assembly_lines.append(&mut result_instruction);
                }
                Err(error) => {
                    return Err(DebuggerError::Other(anyhow!(error)));
                }
            };
        }

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
        request: Request,
    ) -> Result<()> {
        let arguments: DisassembleArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };
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
                    self.send_response::<()>(request, Err(DebuggerError::Other(anyhow!(error))))
                }
            }
        } else {
            self.send_response::<()>(
                request,
                Err(DebuggerError::Other(anyhow!(
                    "Invalid memory reference {:?}",
                    arguments.memory_reference
                ))),
            )
        }
    }

    /// The MS DAP Specification only gives us the unique reference of the variable, and does not tell us which StackFrame it belongs to, nor does it specify if this variable is in the local, register or static scope. Unfortunately this means we have to search through all the available [`probe_rs::debug::variable_cache::VariableCache`]'s until we find it. To minimize the impact of this, we will search in the most 'likely' places first (first stack frame's locals, then statics, then registers, then move to next stack frame, and so on ...)
    pub(crate) fn variables(
        &mut self,
        target_core: &mut CoreHandle,
        request: Request,
    ) -> Result<()> {
        let arguments: VariablesArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };

        if let Some(core_peripherals) = &mut target_core.core_data.core_peripherals {
            // First we check the SVD VariableCache, we do this first because it is the lowest computational overhead.
            if let Some(search_variable) = core_peripherals
                .svd_variable_cache
                .get_variable_by_key(arguments.variables_reference)
            {
                let dap_variables: Vec<Variable> = core_peripherals
                    .svd_variable_cache
                    .get_children(Some(search_variable.variable_key))?
                    .iter_mut()
                    // Convert the `probe_rs::debug::Variable` to `probe_rs_debugger::dap_types::Variable`
                    .map(|variable| {
                        let (
                            variables_reference,
                            named_child_variables_cnt,
                            indexed_child_variables_cnt,
                        ) = self.get_variable_reference(
                            variable,
                            &mut core_peripherals.svd_variable_cache,
                        );
                        Variable {
                            name: if let VariableName::Named(variable_name) = &variable.name {
                                if let Some(last_part) = variable_name.split_terminator('.').last()
                                {
                                    last_part.to_string()
                                } else {
                                    variable_name.to_string()
                                }
                            } else {
                                variable.name.to_string()
                            },
                            // We use fully qualified Peripheral.Register.Field form to ensure the `evaluate` request can find the right registers and fields by name.
                            evaluate_name: Some(variable.name.to_string()),
                            memory_reference: variable
                                .memory_location
                                .memory_address()
                                .map_or_else(
                                    |_| None,
                                    |address| Some(format!("{:#010x}", address)),
                                ),
                            indexed_variables: Some(indexed_child_variables_cnt),
                            named_variables: Some(named_child_variables_cnt),
                            presentation_hint: None,
                            type_: Some(variable.type_name.to_string()),
                            value: {
                                // The SVD cache is not automatically refreshed on every stack trace, and we only need to refresh the field values.
                                variable.extract_value(
                                    &mut target_core.core,
                                    &core_peripherals.svd_variable_cache,
                                );
                                variable.get_value(&core_peripherals.svd_variable_cache)
                            },
                            variables_reference,
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

        let response = {
            let mut parent_variable: Option<probe_rs::debug::Variable> = None;
            let mut variable_cache: Option<&mut probe_rs::debug::VariableCache> = None;
            let mut stack_frame_registers: Option<&DebugRegisters> = None;
            let mut frame_base: Option<u64> = None;
            for stack_frame in target_core.core_data.stack_frames.iter_mut() {
                if let Some(search_cache) = &mut stack_frame.local_variables {
                    if let Some(search_variable) =
                        search_cache.get_variable_by_key(arguments.variables_reference)
                    {
                        parent_variable = Some(search_variable);
                        variable_cache = Some(search_cache);
                        stack_frame_registers = Some(&stack_frame.registers);
                        frame_base = stack_frame.frame_base;
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
                        frame_base = stack_frame.frame_base;
                        break;
                    }
                }

                if stack_frame.id == arguments.variables_reference {
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

            // During the intial stack unwind operation, if encounter [Variable]'s with [VariableNodeType::is_deferred()], they will not be auto-expanded and included in the variable cache.
            // TODO: Use the DAP "Invalidated" event to refresh the variables for this stackframe. It will allow the UI to see updated compound values for pointer variables based on the newly resolved children.
            if let Some(variable_cache) = variable_cache {
                if let Some(parent_variable) = parent_variable.as_mut() {
                    if parent_variable.variable_node_type.is_deferred()
                        && !variable_cache.has_children(parent_variable)?
                    {
                        if let Some(stack_frame_registers) = stack_frame_registers {
                            target_core.core_data.debug_info.cache_deferred_variables(
                                variable_cache,
                                &mut target_core.core,
                                parent_variable,
                                stack_frame_registers,
                                frame_base,
                            )?;
                        } else {
                            tracing::error!("Could not cache deferred child variables for variable: {}. No register data available.", parent_variable.name);
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
                        ) = self.get_variable_reference(variable, variable_cache);
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
                            type_: Some(format!("{:?}", variable.type_name)),
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

    pub(crate) fn r#continue(
        &mut self,
        target_core: &mut CoreHandle,
        request: Request,
    ) -> Result<()> {
        match target_core.core.run() {
            Ok(_) => {
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
                match if target_core.core_data.breakpoints.is_empty() {
                    target_core
                        .core
                        .wait_for_core_halted(Duration::from_millis(200))
                } else {
                    // Use slightly longer timeout when we know there breakpoints configured.
                    target_core
                        .core
                        .wait_for_core_halted(Duration::from_millis(500))
                } {
                    Ok(_) => {
                        // The core has halted, so we can proceed.
                    }
                    Err(wait_error) => {
                        if matches!(
                            wait_error,
                            Error::Arm(ArmError::Timeout) | Error::Riscv(RiscvError::Timeout)
                        ) {
                            // The core is still running.
                        } else {
                            // Some other error occured, so we have to send an error response.
                            return Err(wait_error.into());
                        }
                    }
                }

                Ok(())
            }
            Err(error) => {
                self.send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error))))?;
                Err(error.into())
            }
        }
    }

    /// Steps through the code at the requested granularity.
    /// - [SteppingMode::StepInstruction]: If MS DAP [SteppingGranularity::Instruction] (usually sent from the disassembly view)
    /// - [SteppingMode::OverStatement]: In all other cases.
    pub(crate) fn next(&mut self, target_core: &mut CoreHandle, request: Request) -> Result<()> {
        let arguments: NextArguments = get_arguments(&request)?;

        let stepping_granularity = match arguments.granularity {
            Some(SteppingGranularity::Instruction) => SteppingMode::StepInstruction,
            _ => SteppingMode::OverStatement,
        };

        self.debug_step(stepping_granularity, target_core, request)
    }

    /// Steps through the code at the requested granularity.
    /// - [SteppingMode::StepInstruction]: If MS DAP [SteppingGranularity::Instruction] (usually sent from the disassembly view)
    /// - [SteppingMode::IntoStatement]: In all other cases.
    pub(crate) fn step_in(&mut self, target_core: &mut CoreHandle, request: Request) -> Result<()> {
        let arguments: StepInArguments = get_arguments(&request)?;

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
        request: Request,
    ) -> Result<()> {
        let arguments: StepOutArguments = get_arguments(&request)?;

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
        request: Request,
    ) -> Result<(), anyhow::Error> {
        target_core.reset_core_status(self);
        let (new_status, program_counter) = match stepping_granularity
            .step(&mut target_core.core, &target_core.core_data.debug_info)
        {
            Ok((new_status, program_counter)) => (new_status, program_counter),
            Err(error) => match &error {
                probe_rs::debug::DebugError::NoValidHaltLocation {
                    message,
                    pc_at_error,
                } => {
                    self.show_message(
                        MessageSeverity::Information,
                        format!("Step error @{:#010X}: {}", pc_at_error, message),
                    );
                    (target_core.core.status()?, *pc_at_error)
                }
                other_error => {
                    target_core.core.halt(Duration::from_millis(100)).ok();
                    return Err(anyhow!("Unexpected error during stepping :{}", other_error));
                }
            },
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

    /// The DAP protocol uses three related values to determine how to invoke the `Variables` request.
    /// This function retrieves that information from the `DebugInfo::VariableCache` and returns it as
    /// (`variable_reference`, `named_child_variables_cnt`, `indexed_child_variables_cnt`)
    fn get_variable_reference(
        &mut self,
        parent_variable: &probe_rs::debug::Variable,
        cache: &mut probe_rs::debug::VariableCache,
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
        let expanded_error = {
            let mut response_message = response.to_string();
            let mut offset_iterations = 0;
            let mut child_error: Option<&dyn std::error::Error> =
                std::error::Error::source(&response);
            while let Some(source_error) = child_error {
                offset_iterations += 1;
                response_message = format!("{}\n", response_message,);
                for _offset_counter in 0..offset_iterations {
                    response_message = format!("{}\t", response_message);
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
        progress: Option<f64>,
        message: Option<impl Into<String>>,
        progress_id: i64,
    ) -> Result<ProgressId> {
        anyhow::ensure!(
            self.supports_progress_reporting,
            "Progress reporting is not supported by client."
        );

        self.send_event(
            "progressUpdate",
            Some(ProgressUpdateEventBody {
                message: message.map(|v| v.into()),
                percentage: progress.map(|progress| progress * 100.0),
                progress_id: progress_id.to_string(),
            }),
        )?;

        Ok(progress_id)
    }

    pub(crate) fn set_console_log_level(&mut self, error: ConsoleLog) {
        self.adapter.set_console_log_level(error)
    }
}

/// A helper function to greate a [`dap_types::Source`] struct from a [`SourceLocation`]
fn get_dap_source(source_location: &SourceLocation) -> Option<Source> {
    // Attempt to construct the path for the source code
    source_location.directory.as_ref().map(|path| {
        let mut path = if path.is_relative() {
            if let Ok(current_path) = std::env::current_dir() {
                current_path.join(path)
            } else {
                path.to_owned()
            }
        } else {
            path.to_owned()
        };

        if let Some(file) = &source_location.file {
            path.push(file);
        }

        if path.exists() {
            Source {
                name: source_location.file.clone(),
                path: Some(path.to_string_lossy().to_string()),
                source_reference: None,
                presentation_hint: None,
                origin: None,
                sources: None,
                adapter_data: None,
                checksums: None,
            }
        } else {
            Source {
                name: source_location
                    .file
                    .clone()
                    .map(|file_name| format!("<unavailable>: {}", file_name)),
                path: Some(path.to_string_lossy().to_string()),
                source_reference: None,
                presentation_hint: Some("deemphasize".to_string()),
                origin: None,
                sources: None,
                adapter_data: None,
                checksums: None,
            }
        }
    })
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
    fn short_long_status(&self, program_counter: Option<u64>) -> (&'static str, String);
}
impl DapStatus for CoreStatus {
    /// Return a tuple with short and long descriptions of the core status for human machine interface / hmi. The short status matches with the strings implemented by the Microsoft DAP protocol, e.g. `let (short_status, long status) = CoreStatus::short_long_status(core_status)`
    fn short_long_status(&self, program_counter: Option<u64>) -> (&'static str, String) {
        match self {
            CoreStatus::Running => ("continued", "Core is running".to_string()),
            CoreStatus::Sleeping => ("sleeping", "Core is in SLEEP mode".to_string()),
            CoreStatus::LockedUp => (
                "lockedup",
                "Core is in LOCKUP status - encountered an unrecoverable exception".to_string(),
            ),
            CoreStatus::Halted(halt_reason) => match halt_reason {
                HaltReason::Breakpoint(cause) => (
                    "breakpoint",
                    format!(
                        "Halted on breakpoint ({:?}) @{}.",
                        cause,
                        if let Some(program_counter) = program_counter {
                            format!("{:#010x}", program_counter)
                        } else {
                            "(unspecified location)".to_string()
                        }
                    ),
                ),
                HaltReason::Exception => (
                    "exception",
                    "Core halted due to an exception, e.g. interupt handler".to_string(),
                ),
                HaltReason::Watchpoint => (
                    "data breakpoint",
                    "Core halted due to a watchpoint or data breakpoint".to_string(),
                ),
                HaltReason::Step => (
                    "step",
                    format!(
                        "Halted after a 'step' instruction @{}.",
                        if let Some(program_counter) = program_counter {
                            format!("{:#010x}", program_counter)
                        } else {
                            "(unspecified location)".to_string()
                        }
                    ),
                ),
                HaltReason::Request => (
                    "pause",
                    "Core halted due to a user (debugger client) request".to_string(),
                ),
                HaltReason::External => (
                    "external",
                    "Core halted due to an external request".to_string(),
                ),
                _other => (
                    "unrecognized",
                    "Core halted: unrecognized cause".to_string(),
                ),
            },
            CoreStatus::Unknown => ("unknown", "Core status cannot be determined".to_string()),
        }
    }
}
