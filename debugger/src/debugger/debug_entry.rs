use super::session_data;
use crate::{
    debug_adapter::{
        dap_adapter::*,
        dap_types::*,
        protocol::{DapAdapter, ProtocolAdapter},
    },
    debugger::configuration::{self, ConsoleLog},
    peripherals::svd_variables::SvdCache,
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

    /// The logic of this function is as follows:
    /// - While we are waiting for DAP-Client, we have to continuously check in on the status of the probe.
    /// - Initally, while [`DebugAdapter::configuration_done`] = `false`, we do nothing.
    /// - Once [`DebugAdapter::configuration_done`] = `true`, we can start polling the probe for status, as follows:
    ///   - If the [`super::core_data::CoreData::last_known_status`] is `Halted(_)`, then we stop polling the Probe until the next DAP-Client request attempts an action
    ///   - If the `new_status` is an Err, then the probe is no longer available, and we  end the debugging session
    ///   - If the `new_status` is `Running`, then we have to poll on a regular basis, until the Probe stops for good reasons like breakpoints, or bad reasons like panics.
    pub(crate) fn process_next_request<P: ProtocolAdapter>(
        &mut self,
        session_data: &mut session_data::SessionData,
        debug_adapter: &mut DebugAdapter<P>,
    ) -> Result<DebuggerStatus, DebuggerError> {
        match debug_adapter.listen_for_request()? {
            None => {
                if debug_adapter.all_cores_halted {
                    // Once all cores are halted, then we can skip polling the core for status, and just wait for the next DAP Client request.
                    tracing::trace!(
                        "Sleeping (all cores are halted) for 100ms to reduce polling overheaads."
                    );
                    thread::sleep(Duration::from_millis(100)); // Medium delay to reduce fast looping costs.
                } else {
                    // Poll ALL target cores for status, which includes synching status with the DAP client, and handling RTT data.
                    let (_, suggest_delay_required) =
                        session_data.poll_cores(&self.config, debug_adapter)?;
                    // If there are no requests from the DAP Client, and there was no RTT data in the last poll, then we can sleep for a short period of time to reduce CPU usage.
                    if debug_adapter.configuration_is_done() && suggest_delay_required {
                        tracing::trace!(
                            "Sleeping (core is running) for 50ms to reduce polling overheads."
                        );
                        thread::sleep(Duration::from_millis(50)); // Small delay to reduce fast looping costs.
                    } else {
                        tracing::trace!("Retrieving data from the core, no delay required between iterations of polling the core.");
                    };
                }

                Ok(DebuggerStatus::ContinueSession)
            }
            Some(request) => {
                // Poll ALL target cores for status, which includes synching status with the DAP client, and handling RTT data.
                let (core_statuses, _) = session_data.poll_cores(&self.config, debug_adapter)?;
                // TODO: Currently, we only use `poll_cores()` results from the first core and need to expand to a multi-core implementation that understands which MS DAP requests are core specific.
                if let (core_id, Some(new_status)) = (0_usize, core_statuses.first().cloned()) {
                    // Attach to the core. so that we have the handle available for processing the request.
                    let mut target_core = if let Some(target_core_config) =
                        self.config.core_configs.get_mut(core_id)
                    {
                        if let Ok(core_handle) =
                            session_data.attach_core(target_core_config.core_index)
                        {
                            core_handle
                        } else {
                            return Err(DebuggerError::Other(anyhow!(
                                "Unable to connect to target core"
                            )));
                        }
                    } else {
                        return Err(DebuggerError::Other(anyhow!(
                            "No core configuration found for core id {}",
                            core_id
                        )));
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
                            if new_status == CoreStatus::Sleeping {
                                match target_core.core.halt(Duration::from_millis(100)) {
                                    Ok(_) => {
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
                        _ => {}
                    }

                    // Now we are ready to execute supported commands, or return an error if it isn't supported.
                    match match request.command.clone().as_ref() {
                        "rttWindowOpened" => {
                            if let Some(debugger_rtt_target) =
                                target_core.core_data.rtt_connection.as_mut()
                            {
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
                            .pause(&mut target_core, request)
                            .and(Ok(DebuggerStatus::TerminateSession)),
                        "next" => debug_adapter
                            .next(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "stepIn" => debug_adapter
                            .step_in(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "stepOut" => debug_adapter
                            .step_out(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "pause" => debug_adapter
                            .pause(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "readMemory" => debug_adapter
                            .read_memory(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "writeMemory" => debug_adapter
                            .write_memory(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "setVariable" => debug_adapter
                            .set_variable(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "configurationDone" => debug_adapter
                            .configuration_done(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "threads" => debug_adapter
                            .threads(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "restart" => {
                            // Reset RTT so that the link can be re-established
                            target_core.core_data.rtt_connection = None;
                            debug_adapter
                                .restart(&mut target_core, Some(request))
                                .and(Ok(DebuggerStatus::ContinueSession))
                        }
                        "setBreakpoints" => debug_adapter
                            .set_breakpoints(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "setInstructionBreakpoints" => debug_adapter
                            .set_instruction_breakpoints(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "stackTrace" => debug_adapter
                            .stack_trace(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "scopes" => debug_adapter
                            .scopes(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "disassemble" => debug_adapter
                            .disassemble(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "variables" => debug_adapter
                            .variables(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "continue" => debug_adapter
                            .r#continue(&mut target_core, request)
                            .and(Ok(DebuggerStatus::ContinueSession)),
                        "evaluate" => debug_adapter
                            .evaluate(&mut target_core, request)
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
                                match target_core.core.run() {
                                    Ok(_) => {}
                                    Err(error) => {
                                        debug_adapter.send_error_response(
                                            &DebuggerError::Other(anyhow!("{}", error)),
                                        )?;
                                        return Err(error.into());
                                    }
                                }
                            }
                            Ok(debugger_status)
                        }
                        Err(e) => Err(DebuggerError::Other(e.context("Error executing request."))),
                    }
                } else if debug_adapter.configuration_is_done() {
                    // We've passed `configuration_done` and still do not have at least one core configured.
                    Err(DebuggerError::Other(anyhow!(
                        "Cannot continue unless one target core configuration is defined."
                    )))
                } else {
                    // Keep processing "configuration" requests until we've passed `configuration_done` and have a valid `target_core`.
                    Ok(DebuggerStatus::ContinueSession)
                }
            }
        }

        // Now we can process the next (if any) DAP request.
    }

    /// `debug_session` is where the primary _debug processing_ for the DAP (Debug Adapter Protocol) adapter happens.
    /// All requests are interpreted, actions taken, and responses formulated here. This function is self contained and returns nothing.
    /// The [`DebugAdapter`] takes care of _implementing the DAP Base Protocol_ and _communicating with the DAP client_ and _probe_.
    pub(crate) fn debug_session<P: ProtocolAdapter + 'static>(
        &mut self,
        mut debug_adapter: DebugAdapter<P>,
        log_info_message: &String,
    ) -> Result<DebuggerStatus, DebuggerError> {
        debug_adapter.log_to_console("Starting debug session...");
        debug_adapter.log_to_console(log_info_message);

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

        // TODO: Multi-core: This currently only supports the first `SessionConfig::core_configs`
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
                    self.config.console_log_level.unwrap_or(ConsoleLog::Console),
                );
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

        // Validate file specifications in the config.
        match self.config.validate_config_files() {
            Ok(_) => {}
            Err(error) => {
                return Err(error);
            }
        };

        let mut session_data = match session_data::SessionData::new(&mut self.config) {
            Ok(session_data) => session_data,
            Err(error) => {
                debug_adapter.send_error_response(&error)?;
                return Err(error);
            }
        };

        // TODO: Currently the logic of processing MS DAP requests and executing them, is based on having a single core. It needs to be re-thought for multiple cores. Not all DAP requests require access to the core. One possible is to do the core attach inside each of the request implementations for those that need it, because the applicable core_index can be read from the request arguments.
        // TODO: Until we refactor this, we only support a single core (always the first one specified in `SessionConfig::core_configs`)
        let target_core_config =
            if let Some(target_core_config) = self.config.core_configs.first_mut() {
                target_core_config
            } else {
                return Err(DebuggerError::Other(anyhow!(
                    "Cannot continue unless one target core configuration is defined."
                )));
            };

        debug_adapter.halt_after_reset = self.config.flashing_config.halt_after_reset;
        // Do the flashing.
        // TODO: Multi-core ... needs to flash multiple binaries
        {
            if self.config.flashing_config.flashing_enabled {
                let path_to_elf = match &target_core_config.program_binary {
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
                    "FLASHING: Starting write of {:?} to device memory",
                    &path_to_elf
                ));

                let progress_id = debug_adapter
                    .start_progress("Flashing device", Some(launch_attach_request.seq))
                    .ok();

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
                                            .update_progress(
                                                Some(0.0),
                                                Some("Reading Old Pages ..."),
                                                id,
                                            )
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
                                                Some(progress),
                                                Some(format!("Reading Old Pages ({})", progress)),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FailedFilling => {
                                        debug_adapter
                                            .update_progress(
                                                Some(1.0),
                                                Some("Reading Old Pages Failed!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FinishedFilling => {
                                        debug_adapter
                                            .update_progress(
                                                Some(1.0),
                                                Some("Reading Old Pages Complete!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::StartedErasing => {
                                        debug_adapter
                                            .update_progress(
                                                Some(0.0),
                                                Some("Erasing Sectors ..."),
                                                id,
                                            )
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
                                                Some(progress),
                                                Some(format!("Erasing Sectors ({})", progress)),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FailedErasing => {
                                        debug_adapter
                                            .update_progress(
                                                Some(1.0),
                                                Some("Erasing Sectors Failed!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FinishedErasing => {
                                        debug_adapter
                                            .update_progress(
                                                Some(1.0),
                                                Some("Erasing Sectors Complete!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::StartedProgramming => {
                                        debug_adapter
                                            .update_progress(
                                                Some(0.0),
                                                Some("Programming Pages ..."),
                                                id,
                                            )
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
                                                Some(progress),
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
                                                Some(1.0),
                                                Some("Flashing Pages Failed!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FinishedProgramming => {
                                        debug_adapter
                                            .update_progress(
                                                Some(1.0),
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
                            path_to_elf,
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
                            "FLASHING: Completed write of {:?} to device memory",
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
            let mut target_core = match session_data.attach_core(target_core_config.core_index) {
                Ok(mut target_core) => {
                    // Immediately after attaching, halt the core, so that we can finish initalization without bumping into user code.
                    // Depending on supplied `config`, the core will be restarted at the end of initialization in the `configuration_done` request.
                    match halt_core(&mut target_core.core) {
                        Ok(_) => {
                            // Ensure ebreak enters debug mode, this is necessary for soft breakpoints to work on architectures like RISC-V.
                            target_core.core.debug_on_sw_breakpoint(true)?;
                        }
                        Err(error) => {
                            debug_adapter.send_error_response(&error)?;
                            return Err(error);
                        }
                    }
                    // Before we complete, load the (optional) CMSIS-SVD file and its variable cache.
                    // Configure the [CorePeripherals].
                    if let Some(svd_file) = &target_core_config.svd_file {
                        target_core.core_data.core_peripherals = match SvdCache::new(
                            svd_file,
                            &mut target_core.core,
                            &mut debug_adapter,
                            launch_attach_request.seq,
                        ) {
                            Ok(core_peripherals) => Some(core_peripherals),
                            Err(error) => {
                                tracing::error!("{:?}", error);
                                None
                            }
                        };
                    }
                    target_core
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
                    .restart(&mut target_core, None)
                    .context("Failed to restart core")?;
            }
        }

        // After flashing and forced setup, we can signal the client that are ready to receive incoming requests.
        // Send the `initalized` event to client.
        debug_adapter.send_response::<()>(launch_attach_request, Ok(None))?;
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
                    // All is good. We can process the next request.
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

pub fn debug(port: Option<u16>, vscode: bool, log_info_message: &String) -> Result<()> {
    let mut debugger = Debugger::new(port);

    log_to_console_and_tracing("Starting as a DAP Protocol server".to_string());

    match &debugger.config.port.clone() {
        Some(port) => {
            let addr = std::net::SocketAddr::new(
                std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
                port.to_owned(),
            );

            // Tell the user if (and where) RUST_LOG messages are written.
            log_to_console_and_tracing(log_info_message.to_string());

            loop {
                let listener = TcpListener::bind(addr)?;

                log_to_console_and_tracing(format!(
                    "Listening for requests on port {}",
                    addr.port()
                ));

                listener.set_nonblocking(false).ok();
                match listener.accept() {
                    Ok((socket, addr)) => {
                        socket.set_nonblocking(true).with_context(|| {
                            format!(
                                "Failed to negotiate non-blocking socket with request from :{}",
                                addr
                            )
                        })?;

                        log_to_console_and_tracing(format!("..Starting session from   :{}", addr));

                        let reader = socket
                            .try_clone()
                            .context("Failed to establish a bi-directional Tcp connection.")?;
                        let writer = socket;

                        let dap_adapter = DapAdapter::new(reader, writer);

                        let debug_adapter = DebugAdapter::new(dap_adapter);

                        match debugger.debug_session(debug_adapter, log_info_message) {
                            Err(error) => {
                                tracing::error!("probe-rs-debugger session ended: {}", error);
                            }
                            Ok(DebuggerStatus::TerminateSession) => {
                                log_to_console_and_tracing(format!(
                                    "....Closing session from  :{}",
                                    addr
                                ));
                            }
                            Ok(DebuggerStatus::ContinueSession) => {
                                tracing::error!("probe-rs-debugger enountered unexpected `DebuggerStatus` in debug() execution. Please report this as a bug.");
                            }
                        }
                        // Terminate this process if it was started by VSCode
                        if vscode {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::error!("probe-rs-debugger failed to establish a socket connection. Reason: {:?}", error);
                    }
                }
            }
            log_to_console_and_tracing("CONSOLE: DAP Protocol server exiting".to_string());
        }
        None => {
            tracing::error!("Using probe-rs-debugger as a debug server, requires the use of the `--port` option. Please use the `--help` option for additional information");
        }
    };

    Ok(())
}

/// All eprintln! messages are picked up by the VSCode extension and displayed in the debug console. We send these to stderr, in addition to logging them, so that they will show up, irrespective of the RUST_LOG level filters.
fn log_to_console_and_tracing(message: String) {
    eprintln!("probe-rs-debug: {}", &message);
    tracing::info!("{}", &message);
}
