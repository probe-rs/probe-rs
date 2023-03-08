use super::session_data::SessionData;
use crate::{
    debug_adapter::{
        dap::adapter::*,
        dap::dap_types::*,
        protocol::{DapAdapter, ProtocolAdapter},
    },
    debugger::configuration::{self, ConsoleLog},
    peripherals::svd_variables::SvdCache,
    DebuggerError,
};
use anyhow::{anyhow, Context, Result};
use probe_rs::{
    flashing::{download_file_with_options, DownloadOptions, FlashProgress, Format},
    Architecture, CoreStatus, Probe,
};
use serde::Deserialize;
use std::{
    cell::RefCell,
    fs,
    net::{Ipv4Addr, TcpListener},
    ops::Mul,
    path::Path,
    rc::Rc,
    thread,
    time::{Duration, UNIX_EPOCH},
};
use time::UtcOffset;

#[derive(Copy, Clone, Debug, Deserialize, PartialEq)]
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
                "'{s}' is not a valid target session type. Can be either 'attach' or 'launch']."
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
/// The `DebuggerStatus` is used to control how the Debugger::debug_session() decides if it should respond to DAP Client requests such as `Terminate`, `Disconnect`, and `Reset`, as well as how to repond to unrecoverable errors during a debug session interacting with a target session.
pub(crate) enum DebugSessionStatus {
    Continue,
    Terminate,
    Restart(Request),
}

/// #Debugger Overview
/// The DAP Server will usually be managed automatically by the VSCode client.
/// The DAP Server can optionally be run from the command line as a "server" process.
/// - In this case, the management (start and stop) of the server process is the responsibility of the user. e.g.
///   - `probe-rs-debug --debug --port <IP port number> <other options>` : Uses TCP Sockets to the defined IP port number to service DAP requests.
pub struct Debugger {
    config: configuration::SessionConfig,

    /// UTC offset used for timestamps
    ///
    /// Getting the offset fails in multithreaded programs, so it's
    /// easier to determine it once and then save it.
    timestamp_offset: UtcOffset,

    // TODO: Store somewhere else
    // Timestamp of the flashed binary
    binary_timestamp: Option<Duration>,
}

impl Debugger {
    /// Create a new debugger instance
    pub fn new(timestamp_offset: UtcOffset) -> Self {
        Self {
            config: configuration::SessionConfig::default(),
            timestamp_offset,
            binary_timestamp: None,
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
        session_data: &mut SessionData,
        debug_adapter: &mut DebugAdapter<P>,
    ) -> Result<DebugSessionStatus, DebuggerError> {
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

                Ok(DebugSessionStatus::Continue)
            }
            Some(request) => {
                // Poll ALL target cores for status, which includes synching status with the DAP client, and handling RTT data.
                let (core_statuses, _) = session_data.poll_cores(&self.config, debug_adapter)?;

                // Check if we have configured cores
                if core_statuses.is_empty() {
                    if debug_adapter.configuration_is_done() {
                        // We've passed `configuration_done` and still do not have at least one core configured.
                        return Err(DebuggerError::Other(anyhow!(
                            "Cannot continue unless one target core configuration is defined."
                        )));
                    } else {
                        // Keep processing "configuration" requests until we've passed `configuration_done` and have a valid `target_core`.
                        return Ok(DebugSessionStatus::Continue);
                    }
                }

                // TODO: Currently, we only use `poll_cores()` results from the first core and need to expand to a multi-core implementation that understands which MS DAP requests are core specific.
                let core_id = 0;
                let new_status = &core_statuses[0]; // Checked above

                // Attach to the core. so that we have the handle available for processing the request.

                let Some(target_core_config) = self.config.core_configs.get_mut(core_id) else {
                        return Err(DebuggerError::Other(anyhow!(
                            "No core configuration found for core id {}",
                            core_id
                        )));
                    };

                let Ok(mut target_core) = session_data.attach_core(target_core_config.core_index) else {
                        return Err(DebuggerError::Other(anyhow!(
                            "Unable to connect to target core"
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
                        if new_status == &CoreStatus::Sleeping {
                            match target_core.core.halt(Duration::from_millis(100)) {
                                Ok(_) => {
                                    unhalt_me = true;
                                }
                                Err(error) => {
                                    debug_adapter.send_response::<()>(
                                        &request,
                                        Err(DebuggerError::Other(anyhow!("{}", error))),
                                    )?;
                                    return Err(error.into());
                                }
                            }
                        }
                    }
                    _ => {}
                }

                let mut debug_session = DebugSessionStatus::Continue;

                // Now we are ready to execute supported commands, or return an error if it isn't supported.
                let result = match request.command.clone().as_ref() {
                    "rttWindowOpened" => {
                        if let Some(debugger_rtt_target) =
                            target_core.core_data.rtt_connection.as_mut()
                        {
                            let arguments: RttWindowOpenedArguments =
                                get_arguments(debug_adapter, &request)?;
                            debugger_rtt_target
                                .debugger_rtt_channels
                                .iter_mut()
                                .find(|debugger_rtt_channel| {
                                    debugger_rtt_channel.channel_number == arguments.channel_number
                                })
                                .map_or(false, |rtt_channel| {
                                    rtt_channel.has_client_window = arguments.window_is_open;
                                    arguments.window_is_open
                                });
                            debug_adapter
                                .send_response::<()>(&request, Ok(None))
                                .map_err(|error| {
                                    DebuggerError::Other(anyhow!(
                                        "Could not deserialize arguments for RttWindowOpened : {:?}.",
                                        error
                                    ))
                                })?;
                        }
                        Ok(())
                    }
                    "disconnect" => {
                        let result = debug_adapter.disconnect(&mut target_core, &request);
                        debug_session = DebugSessionStatus::Terminate;
                        result
                    }
                    "next" => debug_adapter.next(&mut target_core, &request),
                    "stepIn" => debug_adapter.step_in(&mut target_core, &request),
                    "stepOut" => debug_adapter.step_out(&mut target_core, &request),
                    "pause" => debug_adapter.pause(&mut target_core, &request),
                    "readMemory" => debug_adapter.read_memory(&mut target_core, &request),
                    "writeMemory" => debug_adapter.write_memory(&mut target_core, &request),
                    "setVariable" => debug_adapter.set_variable(&mut target_core, &request),
                    "configurationDone" => {
                        debug_adapter.configuration_done(&mut target_core, &request)
                    }
                    "threads" => debug_adapter.threads(&mut target_core, &request),
                    "restart" => {
                        if target_core.core.architecture() == Architecture::Riscv {
                            debug_adapter.show_message(
                                MessageSeverity::Information,
                                "In-session `restart` is not currently supported for RISC-V.",
                            );
                            Ok(())
                        } else {
                            // Reset RTT so that the link can be re-established
                            target_core.core_data.rtt_connection = None;
                            let result = target_core
                                .core
                                .halt(Duration::from_millis(500))
                                .map_err(|error| anyhow!("Failed to halt core: {}", error))
                                .and(Ok(()));

                            debug_session = DebugSessionStatus::Restart(request);
                            result
                        }
                    }
                    "setBreakpoints" => debug_adapter.set_breakpoints(&mut target_core, &request),
                    "setInstructionBreakpoints" => {
                        debug_adapter.set_instruction_breakpoints(&mut target_core, &request)
                    }
                    "stackTrace" => debug_adapter.stack_trace(&mut target_core, &request),
                    "scopes" => debug_adapter.scopes(&mut target_core, &request),
                    "disassemble" => debug_adapter.disassemble(&mut target_core, &request),
                    "variables" => debug_adapter.variables(&mut target_core, &request),
                    "continue" => debug_adapter.r#continue(&mut target_core, &request),
                    "evaluate" => debug_adapter.evaluate(&mut target_core, &request),
                    "completions" => debug_adapter.completions(&mut target_core, &request),
                    other_command => {
                        // Unimplemented command.
                        debug_adapter.send_response::<()>(
                            &request,
                            Err(DebuggerError::Other(anyhow!("Received request '{}', which is not supported or not implemented yet", other_command))),)
                            .and(Ok(()))
                    }
                };

                match result {
                    Ok(()) => {
                        if unhalt_me {
                            if let Err(error) = target_core.core.run() {
                                debug_adapter.send_error_response(&DebuggerError::Other(
                                    anyhow!("{}", error),
                                ))?;
                                return Err(error.into());
                            }
                        }

                        Ok(debug_session)
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
        log_info_message: &str,
    ) -> Result<DebugSessionStatus, DebuggerError> {
        debug_adapter.log_to_console("Starting debug session...");
        debug_adapter.log_to_console(log_info_message);

        // The DapClient startup process has a specific sequence.
        // Handle it here before starting a probe-rs session and looping through user generated requests.
        // Handling the initialize, and Attach/Launch requests here in this method,
        // before entering the iterative loop that processes requests through the process_request method.

        // Initialize request
        self.handle_initialize(&mut debug_adapter)?;

        // Process either the Launch or Attach request.
        let (mut debug_adapter, mut session_data) = self.handle_launch_attach(debug_adapter)?;

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
            let debug_session_status = self
                .process_next_request(&mut session_data, &mut debug_adapter)
                .or_else(|e| {
                    debug_adapter.show_message(
                        MessageSeverity::Error,
                        format!("Debug Adapter terminated unexpectedly with an error: {e:?}"),
                    );
                    debug_adapter
                        .send_event("terminated", Some(TerminatedEventBody { restart: None }))?;
                    debug_adapter.send_event("exited", Some(ExitedEventBody { exit_code: 1 }))?;
                    // Keep the process alive for a bit, so that VSCode doesn't complain about broken pipes.
                    for _loop_count in 0..10 {
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(e)
                })?;

            match debug_session_status {
                DebugSessionStatus::Continue => {
                    // All is good. We can process the next request.
                }
                DebugSessionStatus::Restart(request) => {
                    debug_adapter = self.restart(debug_adapter, &mut session_data, &request)?;
                }
                DebugSessionStatus::Terminate => {
                    return Ok(DebugSessionStatus::Terminate);
                }
            };
        }
    }

    /// Process launch or attach request
    fn handle_launch_attach<P: ProtocolAdapter + 'static>(
        &mut self,
        mut debug_adapter: DebugAdapter<P>,
    ) -> Result<(DebugAdapter<P>, SessionData), DebuggerError> {
        let launch_attach_request = loop {
            if let Some(request) = debug_adapter.listen_for_request()? {
                break request;
            }
        };

        let requested_target_session_type = match launch_attach_request.command.as_str() {
            "attach" => TargetSessionType::AttachRequest,
            "launch" => TargetSessionType::LaunchRequest,
            other => {
                let error_msg =
                    format!("Expected request 'launch' or 'attach', but received '{other}'");

                debug_adapter.send_response::<()>(
                    &launch_attach_request,
                    Err(DebuggerError::Other(anyhow!(error_msg.clone()))),
                )?;
                return Err(DebuggerError::Other(anyhow!(error_msg)));
            }
        };

        let arguments = get_arguments(&mut debug_adapter, &launch_attach_request)?;

        self.config = configuration::SessionConfig { ..arguments };

        if requested_target_session_type == TargetSessionType::AttachRequest {
            // Since VSCode doesn't do field validation checks for relationships in launch.json request types, check it here.
            if self.config.flashing_config.flashing_enabled
                || self.config.flashing_config.halt_after_reset
                || self.config.flashing_config.full_chip_erase
                || self.config.flashing_config.restore_unwritten_bytes
            {
                debug_adapter.send_response::<()>(
                                        &launch_attach_request,
                                        Err(DebuggerError::Other(anyhow!(
                                            "Please do not use any of the `flashing_enabled`, `reset_after_flashing`, halt_after_reset`, `full_chip_erase`, or `restore_unwritten_bytes` options when using `attach` request type."))),
                                    )?;

                return Err(DebuggerError::Other(anyhow!(
                                            "Please do not use any of the `flashing_enabled`, `reset_after_flashing`, halt_after_reset`, `full_chip_erase`, or `restore_unwritten_bytes` options when using `attach` request type.")));
            }
        }

        debug_adapter
            .set_console_log_level(self.config.console_log_level.unwrap_or(ConsoleLog::Console));

        if let Err(e) = self.config.validate_config_files() {
            let err = anyhow!("{e:?}");

            debug_adapter.send_response::<()>(&launch_attach_request, Err(e))?;
            return Err(err.into());
        }

        let mut session_data =
            SessionData::new(&mut self.config, self.timestamp_offset).or_else(|error| {
                debug_adapter.send_error_response(&error)?;
                Err(error)
            })?;

        debug_adapter.halt_after_reset = self.config.flashing_config.halt_after_reset;

        if self.config.flashing_config.flashing_enabled {
            let target_core_config = self.config.core_configs.first_mut().ok_or_else(|| {
                DebuggerError::Other(anyhow!(
                    "Cannot continue unless one target core configuration is defined."
                ))
            })?;
            let Some(path_to_elf) = target_core_config.program_binary.clone() else {
                    let err =  DebuggerError::Other(anyhow!("Please specify use the `program-binary` option in `launch.json` to specify an executable"));

                    debug_adapter.send_error_response(&err)?;
                    return Err(err);
                };

            // Store timestamp of flashed binary
            self.binary_timestamp = get_file_timestamp(&path_to_elf);

            debug_adapter = self.flash(
                &path_to_elf,
                debug_adapter,
                launch_attach_request.seq,
                &mut session_data,
            )?;
        }

        let target_core_config = self.config.core_configs.first_mut().ok_or_else(|| {
            DebuggerError::Other(anyhow!(
                "Cannot continue unless one target core configuration is defined."
            ))
        })?;

        // First, attach to the core
        let mut target_core = session_data
            .attach_core(target_core_config.core_index)
            .or_else(|error| {
                debug_adapter.send_error_response(&error)?;
                Err(error)
            })?;

        // Immediately after attaching, halt the core, so that we can finish initalization without bumping into user code.
        // Depending on supplied `config`, the core will be restarted at the end of initialization in the `configuration_done` request.
        if let Err(error) = halt_core(&mut target_core.core) {
            debug_adapter.send_error_response(&error)?;
            return Err(error);
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

        if requested_target_session_type == TargetSessionType::LaunchRequest {
            // This will effectively do a `reset` and `halt` of the core, which is what we want until after the `configuration_done` request.
            debug_adapter
                .restart(&mut target_core, None)
                .context("Failed to restart core")?;
        } else {
            // Ensure ebreak enters debug mode, this is necessary for soft breakpoints to work on architectures like RISC-V.
            // For LaunchRequest, this is done in the `restart` above.
            target_core.core.debug_on_sw_breakpoint(true)?;
        }

        drop(target_core);

        debug_adapter.send_response::<()>(&launch_attach_request, Ok(None))?;

        Ok((debug_adapter, session_data))
    }

    fn restart<P: ProtocolAdapter + 'static>(
        &mut self,
        mut debug_adapter: DebugAdapter<P>,
        session_data: &mut SessionData,
        request: &Request,
    ) -> Result<DebugAdapter<P>, DebuggerError> {
        if self.config.flashing_config.flashing_enabled {
            let target_core_config = self.config.core_configs.first_mut().ok_or_else(|| {
                DebuggerError::Other(anyhow!(
                    "Cannot continue unless one target core configuration is defined."
                ))
            })?;
            let Some(path_to_elf) = target_core_config.program_binary.clone() else {
                    let err =  DebuggerError::Other(anyhow!("Please specify use the `program-binary` option in `launch.json` to specify an executable"));

                    debug_adapter.send_error_response(&err)?;
                    return Err(err);
                };

            if is_file_newer(&mut self.binary_timestamp, &path_to_elf) {
                // If there is a new binary as part of a restart, there are some key things that
                // need to be 'reset' for things to work properly.
                session_data.load_debug_info_for_core(target_core_config)?;
                session_data
                    .attach_core(target_core_config.core_index)
                    .map(|mut target_core| target_core.recompute_breakpoints())??;

                debug_adapter =
                    self.flash(&path_to_elf, debug_adapter, request.seq, session_data)?;
            }
        }

        let target_core_config = self.config.core_configs.first_mut().ok_or_else(|| {
            DebuggerError::Other(anyhow!(
                "Cannot continue unless one target core configuration is defined."
            ))
        })?;

        // First, attach to the core
        let mut target_core = session_data
            .attach_core(target_core_config.core_index)
            .or_else(|error| {
                debug_adapter.send_error_response(&error)?;
                Err(error)
            })?;

        // Immediately after attaching, halt the core, so that we can finish restart logic without bumping into user code.
        if let Err(error) = halt_core(&mut target_core.core) {
            debug_adapter.send_error_response(&error)?;
            return Err(error);
        }

        // After completing optional flashing and other config, we can run the debug adapter's restart logic.
        debug_adapter
            .restart(&mut target_core, Some(request))
            .context("Failed to restart core")?;

        Ok(debug_adapter)
    }

    /// Flash the given binary, and report the progress to the
    /// debug adapter.
    ///
    /// The request_id is used to associate the progress with a given debug adapter request.
    fn flash<P: ProtocolAdapter + 'static>(
        &mut self,
        path_to_elf: &Path,
        mut debug_adapter: DebugAdapter<P>,
        request_id: i64,
        session_data: &mut SessionData,
    ) -> Result<DebugAdapter<P>, DebuggerError> {
        debug_adapter.log_to_console(format!(
            "FLASHING: Starting write of {:?} to device memory",
            &path_to_elf
        ));
        let progress_id = debug_adapter
            .start_progress("Flashing device", Some(request_id))
            .ok();

        let mut download_options = DownloadOptions::default();
        download_options.keep_unwritten_bytes = self.config.flashing_config.restore_unwritten_bytes;
        download_options.do_chip_erase = self.config.flashing_config.full_chip_erase;

        let rc_debug_adapter = Rc::new(RefCell::new(debug_adapter));
        let rc_debug_adapter_clone = rc_debug_adapter.clone();

        struct ProgressState {
            total_page_size: usize,
            total_sector_size: usize,
            total_fill_size: usize,
            page_size_done: usize,
            sector_size_done: usize,
            fill_size_done: usize,
        }

        let progress_state = Rc::new(RefCell::new(ProgressState {
            total_page_size: 0,
            total_sector_size: 0,
            total_fill_size: 0,
            page_size_done: 0,
            sector_size_done: 0,
            fill_size_done: 0,
        }));

        let flash_progress = progress_id.map(|id| {
            FlashProgress::new(move |event| {
                let mut flash_progress = progress_state.borrow_mut();
                let mut debug_adapter = rc_debug_adapter_clone.borrow_mut();
                match event {
                    probe_rs::flashing::ProgressEvent::Initialized { flash_layout } => {
                        flash_progress.total_page_size =
                            flash_layout.pages().iter().map(|s| s.size() as usize).sum();

                        flash_progress.total_sector_size = flash_layout
                            .sectors()
                            .iter()
                            .map(|s| s.size() as usize)
                            .sum();

                        flash_progress.total_fill_size =
                            flash_layout.fills().iter().map(|s| s.size() as usize).sum();
                    }
                    probe_rs::flashing::ProgressEvent::StartedFilling => {
                        debug_adapter
                            .update_progress(Some(0.0), Some("Reading Old Pages ..."), id)
                            .ok();
                    }
                    probe_rs::flashing::ProgressEvent::PageFilled { size, .. } => {
                        flash_progress.fill_size_done += size as usize;
                        let progress = flash_progress.fill_size_done as f64
                            / flash_progress.total_fill_size as f64;

                        debug_adapter
                            .update_progress(
                                Some(progress),
                                Some(format!("Reading Old Pages ({progress})")),
                                id,
                            )
                            .ok();
                    }
                    probe_rs::flashing::ProgressEvent::FailedFilling => {
                        debug_adapter
                            .update_progress(Some(1.0), Some("Reading Old Pages Failed!"), id)
                            .ok();
                    }
                    probe_rs::flashing::ProgressEvent::FinishedFilling => {
                        debug_adapter
                            .update_progress(Some(1.0), Some("Reading Old Pages Complete!"), id)
                            .ok();
                    }
                    probe_rs::flashing::ProgressEvent::StartedErasing => {
                        debug_adapter
                            .update_progress(Some(0.0), Some("Erasing Sectors ..."), id)
                            .ok();
                    }
                    probe_rs::flashing::ProgressEvent::SectorErased { size, .. } => {
                        flash_progress.sector_size_done += size as usize;
                        let progress = flash_progress.sector_size_done as f64
                            / flash_progress.total_sector_size as f64;
                        debug_adapter
                            .update_progress(
                                Some(progress),
                                Some(format!("Erasing Sectors ({progress})")),
                                id,
                            )
                            .ok();
                    }
                    probe_rs::flashing::ProgressEvent::FailedErasing => {
                        debug_adapter
                            .update_progress(Some(1.0), Some("Erasing Sectors Failed!"), id)
                            .ok();
                    }
                    probe_rs::flashing::ProgressEvent::FinishedErasing => {
                        debug_adapter
                            .update_progress(Some(1.0), Some("Erasing Sectors Complete!"), id)
                            .ok();
                    }
                    probe_rs::flashing::ProgressEvent::StartedProgramming => {
                        debug_adapter
                            .update_progress(Some(0.0), Some("Programming Pages ..."), id)
                            .ok();
                    }
                    probe_rs::flashing::ProgressEvent::PageProgrammed { size, .. } => {
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
                            .update_progress(Some(1.0), Some("Flashing Pages Failed!"), id)
                            .ok();
                    }
                    probe_rs::flashing::ProgressEvent::FinishedProgramming => {
                        debug_adapter
                            .update_progress(Some(1.0), Some("Flashing Pages Complete!"), id)
                            .ok();
                    }
                    probe_rs::flashing::ProgressEvent::DiagnosticMessage { .. } => (),
                }
            })
        });

        download_options.progress = flash_progress;

        let flash_result = download_file_with_options(
            &mut session_data.session,
            path_to_elf,
            Format::Elf,
            download_options,
        );

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

        match flash_result {
            Ok(_) => {
                debug_adapter.log_to_console(format!(
                    "FLASHING: Completed write of {:?} to device memory",
                    &path_to_elf
                ));
                Ok(debug_adapter)
            }
            Err(error) => {
                let error = DebuggerError::FileDownload(error);
                debug_adapter.send_error_response(&error)?;
                Err(error)
            }
        }
    }

    fn handle_initialize<P: ProtocolAdapter>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
    ) -> Result<(), DebuggerError> {
        let initialize_request = expect_request(debug_adapter, "initialize")?;

        let initialize_arguments =
            get_arguments::<InitializeRequestArguments, _>(debug_adapter, &initialize_request)?;

        if !(initialize_arguments.columns_start_at_1.unwrap_or(true)
            && initialize_arguments.lines_start_at_1.unwrap_or(true))
        {
            debug_adapter.send_response::<()>(
                &initialize_request,
                Err(DebuggerError::Other(anyhow!(
                    "Unsupported Capability: Client requested column and row numbers start at 0."
                ))),
            )?;
            return Err(DebuggerError::Other(anyhow!(
                "Unsupported Capability: Client requested column and row numbers start at 0."
            )));
        }

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
            support_terminate_debuggee: Some(true),
            support_suspend_debuggee: Some(true),
            supports_delayed_stack_trace_loading: Some(true),
            supports_read_memory_request: Some(true),
            supports_write_memory_request: Some(true),
            supports_set_variable: Some(true),
            supports_clipboard_context: Some(true),
            supports_disassemble_request: Some(true),
            supports_instruction_breakpoints: Some(true),
            supports_stepping_granularity: Some(true),
            supports_completions_request: Some(true),
            // supports_value_formatting_options: Some(true),
            // supports_function_breakpoints: Some(true),
            // TODO: Use DEMCR register to implement exception breakpoints
            // supports_exception_options: Some(true),
            // supports_exception_filter_options: Some (true),
            ..Default::default()
        };
        debug_adapter.send_response(&initialize_request, Ok(Some(capabilities)))?;

        Ok(())
    }
}

/// Wait for the next request with the given command.
///
/// If the next request doesn *not* have the given command,
/// the function returns an error.
fn expect_request<P: ProtocolAdapter>(
    debug_adapter: &mut DebugAdapter<P>,
    expected_command: &str,
) -> Result<Request, DebuggerError> {
    let next_request = loop {
        if let Some(current_request) = debug_adapter.listen_for_request()? {
            break current_request;
        };
    };

    if next_request.command == expected_command {
        Ok(next_request)
    } else {
        debug_adapter.send_response::<()>(
            &next_request,
            Err(anyhow!(
                "Initial command was '{}', expected '{}'",
                next_request.command,
                expected_command
            )
            .into()),
        )?;

        Err(DebuggerError::Other(anyhow!(
            "Initial command was '{}', expected '{}'",
            next_request.command,
            expected_command
        )))
    }
}

pub fn list_connected_devices() -> Result<()> {
    let connected_devices = Probe::list_all();

    if !connected_devices.is_empty() {
        println!("The following devices were found:");
        connected_devices
            .iter()
            .enumerate()
            .for_each(|(num, device)| println!("[{num}]: {device:?}"));
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

pub fn debug(
    port: u16,
    vscode: bool,
    log_info_message: &str,
    timestamp_offset: UtcOffset,
) -> Result<()> {
    let mut debugger = Debugger::new(timestamp_offset);

    log_to_console_and_tracing("Starting as a DAP Protocol server");

    let addr = std::net::SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), port);

    // Tell the user if (and where) RUST_LOG messages are written.
    log_to_console_and_tracing(log_info_message);

    loop {
        let listener = TcpListener::bind(addr)?;

        log_to_console_and_tracing(&format!("Listening for requests on port {}", addr.port()));

        listener.set_nonblocking(false)?;

        match listener.accept() {
            Ok((socket, addr)) => {
                socket.set_nonblocking(true).with_context(|| {
                    format!("Failed to negotiate non-blocking socket with request from :{addr}")
                })?;

                log_to_console_and_tracing(&format!("..Starting session from   :{addr}"));

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
                    Ok(DebugSessionStatus::Terminate) => {
                        log_to_console_and_tracing(&format!("....Closing session from  :{addr}"));
                    }
                    Ok(DebugSessionStatus::Continue) | Ok(DebugSessionStatus::Restart(_)) => {
                        tracing::error!("probe-rs-debugger enountered unexpected `DebuggerStatus` in debug() execution. Please report this as a bug.");
                    }
                }
                // Terminate this process if it was started by VSCode
                if vscode {
                    break;
                }
            }
            Err(error) => {
                tracing::error!(
                    "probe-rs-debugger failed to establish a socket connection. Reason: {:?}",
                    error
                );
            }
        }
    }
    log_to_console_and_tracing("CONSOLE: DAP Protocol server exiting");

    Ok(())
}

/// All eprintln! messages are picked up by the VSCode extension and displayed in the debug console. We send these to stderr, in addition to logging them, so that they will show up, irrespective of the RUST_LOG level filters.
fn log_to_console_and_tracing(message: &str) {
    eprintln!("probe-rs-debug: {}", &message);
    tracing::info!("{}", &message);
}

/// Try to get the timestamp of a file.
///
/// If an error occurs, None is returned.
fn get_file_timestamp(path_to_elf: &Path) -> Option<Duration> {
    fs::metadata(path_to_elf)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
}

fn is_file_newer(saved_binary_timestamp: &mut Option<Duration>, path_to_elf: &Path) -> bool {
    if let Some(check_current_binary_timestamp) = *saved_binary_timestamp {
        // We have a timestamp for the binary that is currently on the device, so we need to compare it with the new binary.
        if let Some(new_binary_timestamp) = get_file_timestamp(path_to_elf) {
            // If it is newer, we can flash it. Otherwise just skip flashing.
            if new_binary_timestamp > check_current_binary_timestamp {
                *saved_binary_timestamp = Some(new_binary_timestamp);
                true
            } else {
                false
            }
        } else {
            // For some reason we couldn't get a timestamp for the new binary. Warn and assume it is new.
            tracing::warn!("Could not get timestamp for new binary. Assuming it is new.");
            true
        }
    } else {
        // We don't have a timestamp for the binary that is currently on the device, so we can flash the binary.
        *saved_binary_timestamp = fs::metadata(path_to_elf)
            .and_then(|metadata| metadata.modified())
            .map(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .ok()
            .flatten();
        true
    }
}
