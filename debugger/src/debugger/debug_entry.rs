use super::session_data;
use crate::{
    debug_adapter::{
        dap_adapter::*,
        dap_types::*,
        protocol::{DapAdapter, ProtocolAdapter},
    },
    debugger::configuration::{self, ConsoleLog},
    DebuggerError,
};
use anyhow::{anyhow, Context, Result};
use probe_rs::{
    flashing::{download_file_with_options, DownloadOptions, FlashProgress, Format},
    CoreStatus, Probe,
};
use serde::Deserialize;
use std::{
    cell::RefCell,
    net::{Ipv4Addr, TcpListener},
    ops::Mul,
    rc::Rc,
    thread,
    time::Duration,
};

#[derive(clap::Parser, Copy, Clone, Debug, Deserialize)]
pub(crate) enum TargetSessionType {
    AttachRequest,
    LaunchRequest,
}

impl std::str::FromStr for TargetSessionType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_ascii_lowercase()[..] {
            "attach" => Ok(TargetSessionType::AttachRequest),
            "launch" => Ok(TargetSessionType::LaunchRequest),
            _ => Err(format!(
                "'{}' is not a valid target session type. Can be either 'attach' or 'launch'].",
                s
            )),
        }
    }
}

#[derive(Debug)]
/// The `DebuggerStatus` is used to control how the Debugger::debug_session() decides if it should respond to DAP Client requests such as `Terminate`, `Disconnect`, and `Reset`, as well as how to repond to unrecoverable errors during a debug session interacting with a target session.
pub(crate) enum DebuggerStatus {
    ContinueSession,
    TerminateSession,
}

/// #Debugger Overview
/// The DAP Server will usually be managed automatically by the VSCode client.
/// The DAP Server can optionally be run from the command line as a "server" process.
/// - In this case, the management (start and stop) of the server process is the responsibility of the user. e.g.
///   - `probe-rs-debug --debug --port <IP port number> <other options>` : Uses TCP Sockets to the defined IP port number to service DAP requests.
pub struct Debugger {
    config: configuration::SessionConfig,
}

impl Debugger {
    pub fn new(port: Option<u16>) -> Self {
        Self {
            config: configuration::SessionConfig {
                port,
                ..Default::default()
            },
        }
    }

    pub(crate) fn process_next_request<P: ProtocolAdapter>(
        &mut self,
        session_data: &mut session_data::SessionData,
        debug_adapter: &mut DebugAdapter<P>,
    ) -> Result<DebuggerStatus, DebuggerError> {
        let request = debug_adapter.listen_for_request()?;
        match request {
            None => {
                /*
                The logic of this command is as follows:
                - While we are waiting for DAP-Client (TCP or STDIO), we have to continuously check in on the status of the probe.
                - Initally, while `LAST_KNOWN_STATUS` probe-rs::CoreStatus::Unknown, we do nothing. Wait until latter part of `debug_session` sets it to something known.
                - If the `LAST_KNOWN_STATUS` is `Halted`, then we stop polling the Probe until the next DAP-Client request attempts an action
                - If the `new_status` is an Err, then the probe is no longer available, and we  end the debugging session
                - If the `new_status` is different from the `LAST_KNOWN_STATUS`, then we have to tell the DAP-Client by way of an `Event`
                - If the `new_status` is `Running`, then we have to poll on a regular basis, until the Probe stops for good reasons like breakpoints, or bad reasons like panics. Then tell the DAP-Client.
                */
                match debug_adapter.last_known_status {
                    CoreStatus::Unknown => Ok(DebuggerStatus::ContinueSession), // Don't do anything until we know VSCode's startup sequence is complete, and changes this to either Halted or Running.
                    CoreStatus::Halted(_) => {
                        // Make sure the RTT buffers are drained.
                        match session_data.attach_core(self.config.core_index) {
                            Ok(mut core_data) => {
                                if let Some(rtt_active_target) = &mut core_data.rtt_connection {
                                    rtt_active_target.process_rtt_data(
                                        debug_adapter,
                                        &mut core_data.target_core,
                                    );
                                };
                            }
                            Err(error) => {
                                let _ = debug_adapter.send_error_response(&error)?;
                                return Err(error);
                            }
                        };

                        // No need to poll the target status if we know it is halted and waiting for us to do something.
                        thread::sleep(Duration::from_millis(50)); // Small delay to reduce fast looping costs on the client
                        Ok(DebuggerStatus::ContinueSession)
                    }
                    _other => {
                        let mut received_rtt_data = false;
                        let mut core_data = match session_data.attach_core(self.config.core_index) {
                            Ok(mut core_data) => {
                                // Use every opportunity to poll the RTT channels for data
                                if let Some(rtt_active_target) = &mut core_data.rtt_connection {
                                    received_rtt_data = rtt_active_target.process_rtt_data(
                                        debug_adapter,
                                        &mut core_data.target_core,
                                    );
                                }
                                core_data
                            }
                            Err(error) => {
                                let _ = debug_adapter.send_error_response(&error)?;
                                return Err(error);
                            }
                        };

                        // Check and update the core status.
                        let new_status = match core_data.target_core.status() {
                            Ok(new_status) => new_status,
                            Err(error) => {
                                let error = DebuggerError::ProbeRs(error);
                                let _ = debug_adapter.send_error_response(&error);
                                return Err(error);
                            }
                        };

                        // Only sleep (nap for a short duration) IF the probe's status hasn't changed AND there was no RTT data in the last poll.
                        // Otherwise loop again to keep things flowing as fast as possible.
                        // The justification is that any client side CPU used to keep polling is a small price to pay for maximum throughput of debug requests and RTT from the probe.
                        if received_rtt_data && new_status == debug_adapter.last_known_status {
                            return Ok(DebuggerStatus::ContinueSession);
                        } else if new_status == debug_adapter.last_known_status {
                            thread::sleep(Duration::from_millis(50)); // Small delay to reduce fast looping costs.
                            return Ok(DebuggerStatus::ContinueSession);
                        } else {
                            debug_adapter.last_known_status = new_status;
                        };

                        match new_status {
                            CoreStatus::Running | CoreStatus::Sleeping => {
                                let event_body = Some(ContinuedEventBody {
                                    all_threads_continued: Some(true),
                                    thread_id: core_data.target_core.id() as i64,
                                });
                                debug_adapter.send_event("continued", event_body)?;
                            }
                            CoreStatus::Halted(_) => {
                                let event_body = Some(StoppedEventBody {
                                    reason: new_status.short_long_status().0.to_owned(),
                                    description: Some(new_status.short_long_status().1.to_owned()),
                                    thread_id: Some(core_data.target_core.id() as i64),
                                    preserve_focus_hint: Some(false),
                                    text: None,
                                    all_threads_stopped: Some(true),
                                    hit_breakpoint_ids: None,
                                });
                                debug_adapter.send_event("stopped", event_body)?;
                            }
                            CoreStatus::LockedUp => {
                                debug_adapter.show_message(
                                    MessageSeverity::Error,
                                    new_status.short_long_status().1.to_owned(),
                                );
                                return Err(DebuggerError::Other(anyhow!(new_status
                                    .short_long_status()
                                    .1
                                    .to_owned())));
                            }
                            CoreStatus::Unknown => {
                                debug_adapter.send_error_response(&DebuggerError::Other(
                                    anyhow!("Unknown Device status reveived from Probe-rs"),
                                ))?;

                                return Err(DebuggerError::Other(anyhow!(
                                    "Unknown Device status reveived from Probe-rs"
                                )));
                            }
                        };
                        Ok(DebuggerStatus::ContinueSession)
                    }
                }
            }
            Some(request) => {
                // First, attach to the core.
                let mut core_data = match session_data.attach_core(self.config.core_index) {
                    Ok(core_data) => core_data,
                    Err(error) => {
                        let failed_command = request.command.clone();
                        debug_adapter.send_response::<()>(request, Err(error))?;
                        return Err(DebuggerError::Other(anyhow!(
                            "Error while attaching to core. Could not complete command {}",
                            failed_command
                        )));
                    }
                };

                // For some operations, we need to make sure the core isn't sleeping, by calling `Core::halt()`.
                // When we do this, we need to flag it (`unhalt_me = true`), and later call `Core::run()` again.
                // NOTE: The target will exit sleep mode as a result of this command.
                let mut unhalt_me = false;
                match request.command.as_ref() {
                    "configurationDone"
                    | "setBreakpoint"
                    | "setBreakpoints"
                    | "setInstructionBreakpoints"
                    | "clearBreakpoint"
                    | "stackTrace"
                    | "threads"
                    | "scopes"
                    | "variables"
                    | "readMemory"
                    | "writeMemory"
                    | "disassemble" => {
                        match core_data.target_core.status() {
                            Ok(current_status) => {
                                if current_status == CoreStatus::Sleeping {
                                    match core_data.target_core.halt(Duration::from_millis(100)) {
                                        Ok(_) => {
                                            debug_adapter.last_known_status =
                                                CoreStatus::Halted(probe_rs::HaltReason::Request);
                                            unhalt_me = true;
                                        }
                                        Err(error) => {
                                            debug_adapter.send_response::<()>(
                                                request,
                                                Err(DebuggerError::Other(anyhow!("{}", error))),
                                            )?;
                                            return Err(error.into());
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                let failed_command = request.command.clone();
                                let wrapped_err = DebuggerError::ProbeRs(error);
                                debug_adapter.send_response::<()>(request, Err(wrapped_err))?;

                                // TODO: Nicer response here
                                return Err(DebuggerError::Other(anyhow!(
                                    "Failed to get core status. Could not complete command: {:?}",
                                    failed_command
                                )));
                            }
                        }
                    }
                    _ => {}
                }

                // Now we are ready to execute supported commands, or return an error if it isn't supported.
                match match request.command.clone().as_ref() {
                    "rttWindowOpened" => {
                        if let Some(debugger_rtt_target) = core_data.rtt_connection {
                            match get_arguments::<RttWindowOpenedArguments>(&request) {
                                Ok(arguments) => {
                                    debugger_rtt_target
                                        .debugger_rtt_channels
                                        .iter_mut()
                                        .find(|debugger_rtt_channel| {
                                            debugger_rtt_channel.channel_number
                                                == arguments.channel_number
                                        })
                                        .map_or(false, |rtt_channel| {
                                            rtt_channel.has_client_window =
                                                arguments.window_is_open;
                                            arguments.window_is_open
                                        });
                                    debug_adapter.send_response::<()>(request, Ok(None))?;
                                }
                                Err(error) => {
                                    debug_adapter.send_response::<()>(
                                        request,
                                        Err(DebuggerError::Other(anyhow!(
                                    "Could not deserialize arguments for RttWindowOpened : {:?}.",
                                    error
                                ))),
                                    )?;
                                }
                            }
                        }
                        Ok(DebuggerStatus::ContinueSession)
                    }
                    "disconnect" => debug_adapter
                        .send_response::<()>(request, Ok(None))
                        .and(Ok(DebuggerStatus::TerminateSession)),
                    "terminate" => debug_adapter
                        .pause(&mut core_data, request)
                        .and(Ok(DebuggerStatus::TerminateSession)),
                    "status" => debug_adapter
                        .status(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "next" => debug_adapter
                        .next(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "stepIn" => debug_adapter
                        .step_in(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "stepOut" => debug_adapter
                        .step_out(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "pause" => debug_adapter
                        .pause(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "readMemory" => debug_adapter
                        .read_memory(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "writeMemory" => debug_adapter
                        .write_memory(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "setVariable" => debug_adapter
                        .set_variable(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "configurationDone" => debug_adapter
                        .configuration_done(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "threads" => debug_adapter
                        .threads(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "restart" => {
                        // Reset RTT so that the link can be re-established
                        *core_data.rtt_connection = None;
                        debug_adapter
                            .restart(&mut core_data, Some(request))
                            .and(Ok(DebuggerStatus::ContinueSession))
                    }
                    "setBreakpoints" => debug_adapter
                        .set_breakpoints(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "setInstructionBreakpoints" => debug_adapter
                        .set_instruction_breakpoints(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "stackTrace" => debug_adapter
                        .stack_trace(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "scopes" => debug_adapter
                        .scopes(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "disassemble" => debug_adapter
                        .disassemble(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "variables" => debug_adapter
                        .variables(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "continue" => debug_adapter
                        .r#continue(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "evaluate" => debug_adapter
                        .evaluate(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    other_command => {
                        // Unimplemented command.
                        debug_adapter.send_response::<()>(
                            request,
                            Err(DebuggerError::Other(anyhow!("Received request '{}', which is not supported or not implemented yet", other_command))),)
                            .and(Ok(DebuggerStatus::ContinueSession))
                    }
                } {
                    Ok(debugger_status) => {
                        if unhalt_me {
                            match core_data.target_core.run() {
                                Ok(_) => debug_adapter.last_known_status = CoreStatus::Running,
                                Err(error) => {
                                    debug_adapter.send_error_response(&DebuggerError::Other(
                                        anyhow!("{}", error),
                                    ))?;
                                    return Err(error.into());
                                }
                            }
                        }
                        Ok(debugger_status)
                    }
                    Err(e) => Err(DebuggerError::Other(e.context("Error executing request."))),
                }
            }
        }
    }

    /// `debug_session` is where the primary _debug processing_ for the DAP (Debug Adapter Protocol) adapter happens.
    /// All requests are interpreted, actions taken, and responses formulated here. This function is self contained and returns nothing.
    /// The [`DebugAdapter`] takes care of _implementing the DAP Base Protocol_ and _communicating with the DAP client_ and _probe_.
    pub(crate) fn debug_session<P: ProtocolAdapter + 'static>(
        &mut self,
        mut debug_adapter: DebugAdapter<P>,
    ) -> Result<DebuggerStatus, DebuggerError> {
        // The DapClient startup process has a specific sequence.
        // Handle it here before starting a probe-rs session and looping through user generated requests.
        // Handling the initialize, and Attach/Launch requests here in this method,
        // before entering the iterative loop that processes requests through the process_request method.

        // Initialize request.
        let initialize_request = loop {
            let current_request = if let Some(request) = debug_adapter.listen_for_request()? {
                request
            } else {
                continue;
            };

            match current_request.command.as_str() {
                "initialize" => break current_request, // We have lift off.
                other => {
                    let command = other.to_string();

                    debug_adapter.send_response::<()>(
                        current_request,
                        Err(
                            anyhow!("Initial command was '{}', expected 'initialize'", command)
                                .into(),
                        ),
                    )?;
                    return Err(DebuggerError::Other(anyhow!(
                        "Initial command was '{}', expected 'initialize'",
                        command
                    )));
                }
            };
        };

        let initialize_arguments: InitializeRequestArguments = match get_arguments::<
            InitializeRequestArguments,
        >(&initialize_request)
        {
            Ok(arguments) => {
                if !(arguments.columns_start_at_1.unwrap_or(true)
                    && arguments.lines_start_at_1.unwrap_or(true))
                {
                    debug_adapter.send_response::<()>(initialize_request, Err(DebuggerError::Other(anyhow!("Unsupported Capability: Client requested column and row numbers start at 0."))))?;
                    return Err(DebuggerError::Other(anyhow!("Unsupported Capability: Client requested column and row numbers start at 0.")));
                }
                arguments
            }
            Err(error) => {
                debug_adapter.send_response::<()>(initialize_request, Err(error))?;
                return Err(DebuggerError::Other(anyhow!(
                    "Failed to get initialize arguments"
                )));
            }
        };

        if let Some(progress_support) = initialize_arguments.supports_progress_reporting {
            debug_adapter.supports_progress_reporting = progress_support;
        }

        if let Some(lines_start_at_1) = initialize_arguments.lines_start_at_1 {
            debug_adapter.lines_start_at_1 = lines_start_at_1;
        }

        if let Some(columns_start_at_1) = initialize_arguments.columns_start_at_1 {
            debug_adapter.columns_start_at_1 = columns_start_at_1;
        }

        // Reply to Initialize with `Capabilities`.
        let capabilities = Capabilities {
            supports_configuration_done_request: Some(true),
            supports_restart_request: Some(true),
            supports_terminate_request: Some(true),
            supports_delayed_stack_trace_loading: Some(true),
            supports_read_memory_request: Some(true),
            supports_write_memory_request: Some(true),
            supports_set_variable: Some(true),
            supports_clipboard_context: Some(true),
            supports_disassemble_request: Some(true),
            supports_instruction_breakpoints: Some(true),
            supports_stepping_granularity: Some(true),
            // supports_value_formatting_options: Some(true),
            // supports_function_breakpoints: Some(true),
            // TODO: Use DEMCR register to implement exception breakpoints
            // supports_exception_options: Some(true),
            // supports_exception_filter_options: Some (true),
            ..Default::default()
        };
        debug_adapter.send_response(initialize_request, Ok(Some(capabilities)))?;

        // Process either the Launch or Attach request.
        let requested_target_session_type: Option<TargetSessionType>;
        let launch_attach_request = loop {
            let current_request = if let Some(request) = debug_adapter.listen_for_request()? {
                request
            } else {
                continue;
            };

            match current_request.command.as_str() {
                "attach" => {
                    requested_target_session_type = Some(TargetSessionType::AttachRequest);
                    break current_request;
                }
                "launch" => {
                    requested_target_session_type = Some(TargetSessionType::LaunchRequest);
                    break current_request;
                }
                other => {
                    let error_msg = format!(
                        "Expected request 'launch' or 'attach', but received' {}'",
                        other
                    );

                    debug_adapter.send_response::<()>(
                        current_request,
                        Err(DebuggerError::Other(anyhow!(error_msg.clone()))),
                    )?;
                    return Err(DebuggerError::Other(anyhow!(error_msg)));
                }
            };
        };

        match get_arguments(&launch_attach_request) {
            Ok(arguments) => {
                if requested_target_session_type.is_some() {
                    self.config = configuration::SessionConfig { ..arguments };
                    if matches!(
                        requested_target_session_type,
                        Some(TargetSessionType::AttachRequest)
                    ) {
                        // Since VSCode doesn't do field validation checks for relationships in launch.json request types, check it here.
                        if self.config.flashing_config.flashing_enabled
                            || self.config.flashing_config.reset_after_flashing
                            || self.config.flashing_config.halt_after_reset
                            || self.config.flashing_config.full_chip_erase
                            || self.config.flashing_config.restore_unwritten_bytes
                        {
                            debug_adapter.send_response::<()>(
                                        launch_attach_request,
                                        Err(DebuggerError::Other(anyhow!(
                                            "Please do not use any of the `flashing_enabled`, `reset_after_flashing`, halt_after_reset`, `full_chip_erase`, or `restore_unwritten_bytes` options when using `attach` request type."))),
                                    )?;

                            return Err(DebuggerError::Other(anyhow!(
                                            "Please do not use any of the `flashing_enabled`, `reset_after_flashing`, halt_after_reset`, `full_chip_erase`, or `restore_unwritten_bytes` options when using `attach` request type.")));
                        }
                    }
                }
                debug_adapter.set_console_log_level(
                    self.config.console_log_level.unwrap_or(ConsoleLog::Error),
                );
                // Update the `cwd` and `program_binary`.
                self.config.validate_and_update_cwd(self.config.cwd.clone());
                // Update the `program_binary` and validate that the file exists.
                self.config.program_binary = match self
                    .config
                    .qualify_and_update_os_file_path(self.config.program_binary.clone())
                {
                    Ok(program_binary) => {
                        if !program_binary.is_file() {
                            debug_adapter.send_response::<()>(
                                launch_attach_request,
                                Err(DebuggerError::Other(anyhow!(
                                    "program_binary file {:?} not found.",
                                    program_binary
                                ))),
                            )?;
                            return Err(DebuggerError::Other(anyhow!(
                                "Invalid program binary file specified '{:?}'",
                                program_binary
                            )));
                        }
                        Some(program_binary)
                    }
                    Err(error) => {
                        debug_adapter.send_response::<()>(
                            launch_attach_request,
                            Err(DebuggerError::Other(anyhow!(
                                "Please use the --program-binary option to specify an executable: {:?}", error
                            ))),
                        )?;
                        return Err(DebuggerError::Other(anyhow!(
                            "Please use the --program-binary option to specify an executable"
                        )));
                    }
                };
                // Update the `svd_file` and validate that the file exists.
                // If there is a problem with this file, warn the user and continue with the session.
                self.config.svd_file = match self
                    .config
                    .qualify_and_update_os_file_path(self.config.svd_file.clone())
                {
                    Ok(svd_file) => {
                        if !svd_file.is_file() {
                            debug_adapter.show_message(
                                MessageSeverity::Warning,
                                format!("SVD file {:?} not found.", svd_file),
                            );
                            None
                        } else {
                            Some(svd_file)
                        }
                    }
                    Err(error) => {
                        // SVD file is not mandatory.
                        log::debug!("SVD file not specified: {:?}", &error);
                        None
                    }
                };
                debug_adapter.send_response::<()>(launch_attach_request, Ok(None))?;
            }
            Err(error) => {
                let error_message = format!(
                    "Could not derive SessionConfig from request '{}', with arguments {:?}\n{:?} ",
                    launch_attach_request.command, launch_attach_request.arguments, error
                );
                debug_adapter.send_response::<()>(
                    launch_attach_request,
                    Err(DebuggerError::Other(anyhow!(error_message.clone()))),
                )?;

                return Err(DebuggerError::Other(anyhow!(error_message)));
            }
        };

        let mut session_data = match session_data::SessionData::new(&self.config) {
            Ok(session_data) => session_data,
            Err(error) => {
                debug_adapter.send_error_response(&error)?;
                return Err(error);
            }
        };
        debug_adapter.halt_after_reset = self.config.flashing_config.halt_after_reset;

        // Do the flashing.
        {
            if self.config.flashing_config.flashing_enabled {
                let path_to_elf = match self.config.program_binary.clone() {
                    Some(program_binary) => program_binary,
                    None => {
                        let err = DebuggerError::Other(anyhow!(
                            "Please use the --program-binary option to specify an executable"
                        ));
                        debug_adapter.send_error_response(&err)?;
                        return Err(err);
                    }
                };
                debug_adapter.log_to_console(format!(
                    "INFO: FLASHING: Starting write of {:?} to device memory",
                    &path_to_elf
                ));

                let progress_id = debug_adapter.start_progress("Flashing device", None).ok();

                let mut download_options = DownloadOptions::default();
                download_options.keep_unwritten_bytes =
                    self.config.flashing_config.restore_unwritten_bytes;
                download_options.do_chip_erase = self.config.flashing_config.full_chip_erase;
                let flash_result = {
                    let rc_debug_adapter = Rc::new(RefCell::new(debug_adapter));
                    let rc_debug_adapter_clone = rc_debug_adapter.clone();
                    let flash_result = {
                        struct ProgressState {
                            total_page_size: usize,
                            total_sector_size: usize,
                            total_fill_size: usize,
                            page_size_done: usize,
                            sector_size_done: usize,
                            fill_size_done: usize,
                        }

                        let flash_progress = Rc::new(RefCell::new(ProgressState {
                            total_page_size: 0,
                            total_sector_size: 0,
                            total_fill_size: 0,
                            page_size_done: 0,
                            sector_size_done: 0,
                            fill_size_done: 0,
                        }));

                        let flash_progress = if let Some(id) = progress_id {
                            FlashProgress::new(move |event| {
                                let mut flash_progress = flash_progress.borrow_mut();
                                let mut debug_adapter = rc_debug_adapter_clone.borrow_mut();
                                match event {
                                    probe_rs::flashing::ProgressEvent::Initialized {
                                        flash_layout,
                                    } => {
                                        flash_progress.total_page_size = flash_layout
                                            .pages()
                                            .iter()
                                            .map(|s| s.size() as usize)
                                            .sum();

                                        flash_progress.total_sector_size = flash_layout
                                            .sectors()
                                            .iter()
                                            .map(|s| s.size() as usize)
                                            .sum();

                                        flash_progress.total_fill_size = flash_layout
                                            .fills()
                                            .iter()
                                            .map(|s| s.size() as usize)
                                            .sum();
                                    }
                                    probe_rs::flashing::ProgressEvent::StartedFilling => {
                                        debug_adapter
                                            .update_progress(0.0, Some("Reading Old Pages ..."), id)
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::PageFilled {
                                        size, ..
                                    } => {
                                        flash_progress.fill_size_done += size as usize;
                                        let progress = flash_progress.fill_size_done as f64
                                            / flash_progress.total_fill_size as f64;
                                        debug_adapter
                                            .update_progress(
                                                progress,
                                                Some(format!("Reading Old Pages ({})", progress)),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FailedFilling => {
                                        debug_adapter
                                            .update_progress(
                                                1.0,
                                                Some("Reading Old Pages Failed!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FinishedFilling => {
                                        debug_adapter
                                            .update_progress(
                                                1.0,
                                                Some("Reading Old Pages Complete!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::StartedErasing => {
                                        debug_adapter
                                            .update_progress(0.0, Some("Erasing Sectors ..."), id)
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::SectorErased {
                                        size,
                                        ..
                                    } => {
                                        flash_progress.sector_size_done += size as usize;
                                        let progress = flash_progress.sector_size_done as f64
                                            / flash_progress.total_sector_size as f64;
                                        debug_adapter
                                            .update_progress(
                                                progress,
                                                Some(format!("Erasing Sectors ({})", progress)),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FailedErasing => {
                                        debug_adapter
                                            .update_progress(
                                                1.0,
                                                Some("Erasing Sectors Failed!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FinishedErasing => {
                                        debug_adapter
                                            .update_progress(
                                                1.0,
                                                Some("Erasing Sectors Complete!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::StartedProgramming => {
                                        debug_adapter
                                            .update_progress(0.0, Some("Programming Pages ..."), id)
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::PageProgrammed {
                                        size,
                                        ..
                                    } => {
                                        flash_progress.page_size_done += size as usize;
                                        let progress = flash_progress.page_size_done as f64
                                            / flash_progress.total_page_size as f64;
                                        debug_adapter
                                            .update_progress(
                                                progress,
                                                Some(format!(
                                                    "Programming Pages ({:02.0}%)",
                                                    progress.mul(100_f64)
                                                )),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FailedProgramming => {
                                        debug_adapter
                                            .update_progress(
                                                1.0,
                                                Some("Flashing Pages Failed!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FinishedProgramming => {
                                        debug_adapter
                                            .update_progress(
                                                1.0,
                                                Some("Flashing Pages Complete!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                }
                            })
                        } else {
                            FlashProgress::new(|_event| {})
                        };
                        download_options.progress = Some(&flash_progress);
                        download_file_with_options(
                            &mut session_data.session,
                            &path_to_elf,
                            Format::Elf,
                            download_options,
                        )
                    };
                    debug_adapter = match Rc::try_unwrap(rc_debug_adapter) {
                        Ok(debug_adapter) => debug_adapter.into_inner(),
                        Err(too_many_strong_references) => {
                            let other_error = DebuggerError::Other(anyhow!("Unexpected error while dereferencing the `debug_adapter` (It has {} strong references). Please report this as a bug.", Rc::strong_count(&too_many_strong_references)));
                            return Err(other_error);
                        }
                    };

                    if let Some(id) = progress_id {
                        let _ = debug_adapter.end_progress(id);
                    }
                    flash_result
                };

                match flash_result {
                    Ok(_) => {
                        debug_adapter.log_to_console(format!(
                            "INFO: FLASHING: Completed write of {:?} to device memory",
                            &path_to_elf
                        ));
                    }
                    Err(error) => {
                        let error = DebuggerError::FileDownload(error);
                        debug_adapter.send_error_response(&error)?;
                        return Err(error);
                    }
                }
            }
        }

        // This is the first attach to the requested core. If this one works, all subsequent ones will be no-op requests for a Core reference. Do NOT hold onto this reference for the duration of the session ... that is why this code is in a block of its own.
        {
            // First, attach to the core
            let mut core_data = match session_data.attach_core(self.config.core_index) {
                Ok(mut core_data) => {
                    // Immediately after attaching, halt the core, so that we can finish initalization without bumping into user code.
                    // Depending on supplied `config`, the core will be restarted at the end of initialization in the `configuration_done` request.
                    match halt_core(&mut core_data.target_core) {
                        Ok(_) => {}
                        Err(error) => {
                            debug_adapter.send_error_response(&error)?;
                            return Err(error);
                        }
                    }
                    core_data
                }
                Err(error) => {
                    debug_adapter.send_error_response(&error)?;
                    return Err(error);
                }
            };

            if self.config.flashing_config.flashing_enabled
                && self.config.flashing_config.reset_after_flashing
            {
                debug_adapter
                    .restart(&mut core_data, None)
                    .context("Failed to restart core")?;
            }
        }

        // After flashing and forced setup, we can signal the client that are ready to receive incoming requests.
        // Send the `initalized` event to client.
        if debug_adapter
            .send_event::<Event>("initialized", None)
            .is_err()
        {
            let error =
                DebuggerError::Other(anyhow!("Failed sending 'initialized' event to DAP Client"));

            debug_adapter.send_error_response(&error)?;

            return Err(error);
        }

        // Loop through remaining (user generated) requests and send to the [processs_request] method until either the client or some unexpected behaviour termintates the process.
        loop {
            match self.process_next_request(&mut session_data, &mut debug_adapter) {
                Ok(DebuggerStatus::ContinueSession) => {
                    // Validate and if necessary, initialize the RTT structure.
                    if self.config.rtt.enabled
                        && session_data.rtt_connection.is_none()
                        && !(debug_adapter.last_known_status == CoreStatus::Unknown
                            || debug_adapter.last_known_status.is_halted())
                    // Do not attempt this until we have processed the MSDAP request for "configurationDone" ...
                    {
                        let target_memory_map = session_data.session.target().memory_map.clone();
                        let mut core_data = match session_data.attach_core(self.config.core_index) {
                            Ok(core_data) => core_data,
                            Err(error) => {
                                debug_adapter.send_error_response(&error)?;
                                return Err(error);
                            }
                        };
                        log::info!("Attempting to initialize the RTT.");
                        // RTT can only be initialized if the target application has been allowed to run to the point where it does the RTT initialization.
                        // If the target halts before it processes this code, then this RTT intialization silently fails, and will try again later ...
                        // See `probe-rs-rtt::Rtt` for more information.
                        // We can safely unwrap() program_binary here, because it is validated to exist at startup of the debugger
                        #[allow(clippy::unwrap_used)]
                        core_data.attach_to_rtt(
                            &mut debug_adapter,
                            &target_memory_map,
                            self.config.program_binary.as_ref().unwrap(),
                            &self.config.rtt,
                        )?;
                    }
                }
                Ok(DebuggerStatus::TerminateSession) => {
                    return Ok(DebuggerStatus::TerminateSession);
                }
                Err(e) => {
                    debug_adapter.show_message(
                        MessageSeverity::Error,
                        format!(
                            "Debug Adapter terminated unexpectedly with an error: {:?}",
                            e
                        ),
                    );
                    debug_adapter
                        .send_event("terminated", Some(TerminatedEventBody { restart: None }))?;
                    debug_adapter.send_event("exited", Some(ExitedEventBody { exit_code: 1 }))?;
                    // Keep the process alive for a bit, so that VSCode doesn't complain about broken pipes.
                    for _loop_count in 0..10 {
                        thread::sleep(Duration::from_millis(50));
                    }
                    return Err(e);
                }
            }
        }
    }
}

pub fn list_connected_devices() -> Result<()> {
    let connected_devices = Probe::list_all();

    if !connected_devices.is_empty() {
        println!("The following devices were found:");
        connected_devices
            .iter()
            .enumerate()
            .for_each(|(num, device)| println!("[{}]: {:?}", num, device));
    } else {
        println!("No devices were found.");
    }
    Ok(())
}

pub fn list_supported_chips() -> Result<()> {
    println!("Available chips:");
    for family in
        probe_rs::config::families().map_err(|e| anyhow!("Families could not be read: {:?}", e))?
    {
        println!("{}", &family.name);
        println!("    Variants:");
        for variant in family.variants() {
            println!("        {}", variant.name);
        }
    }

    Ok(())
}

pub fn debug(port: Option<u16>, vscode: bool) -> Result<()> {
    let program_name = clap::crate_name!();

    let mut debugger = Debugger::new(port);

    println!(
        "{} CONSOLE: Starting as a DAP Protocol server",
        &program_name
    );
    match &debugger.config.port.clone() {
        Some(port) => {
            let addr = std::net::SocketAddr::new(
                std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
                port.to_owned(),
            );

            loop {
                let listener = TcpListener::bind(addr)?;

                println!(
                    "{} CONSOLE: Listening for requests on port {}",
                    &program_name,
                    addr.port()
                );

                listener.set_nonblocking(false).ok();
                match listener.accept() {
                    Ok((socket, addr)) => {
                        socket.set_nonblocking(true).with_context(|| {
                            format!(
                                "Failed to negotiate non-blocking socket with request from :{}",
                                addr
                            )
                        })?;

                        let message =
                            format!("{}: ..Starting session from   :{}", &program_name, addr);
                        log::info!("{}", &message);
                        println!("{}", &message);
                        let reader = socket
                            .try_clone()
                            .context("Failed to establish a bi-directional Tcp connection.")?;
                        let writer = socket;

                        let dap_adapter = DapAdapter::new(reader, writer);

                        let debug_adapter = DebugAdapter::new(dap_adapter);

                        match debugger.debug_session(debug_adapter) {
                            Err(_) | Ok(DebuggerStatus::TerminateSession) => {
                                println!(
                                    "{} CONSOLE: ....Closing session from  :{}",
                                    &program_name, addr
                                );
                            }
                            Ok(DebuggerStatus::ContinueSession) => {
                                log::error!("probe-rs-debugger enountered unexpected `DebuggerStatus` in debug() execution. Please report this as a bug.");
                            }
                        }
                        // Terminate this process if it was started by VSCode
                        if vscode {
                            break;
                        }
                    }
                    Err(error) => {
                        log::error!("probe-rs-debugger failed to establish a socket connection. Reason: {:?}", error);
                    }
                }
            }
            println!("{} CONSOLE: DAP Protocol server exiting", &program_name);
        }
        None => {
            log::error!("Using probe-rs-debugger as a debug server, requires the use of the `--port` option. Please use the `--help` option for additional information");
        }
    };

    Ok(())
}
