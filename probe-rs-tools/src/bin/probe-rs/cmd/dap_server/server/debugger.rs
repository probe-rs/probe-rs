use super::{
    configuration::{self, ConsoleLog},
    logger::DebugLogger,
    session_data::SessionData,
    startup::{TargetSessionType, get_file_timestamp},
};
use crate::{
    cmd::dap_server::{
        DebuggerError,
        debug_adapter::{
            dap::{
                adapter::{DebugAdapter, get_arguments},
                dap_types::{
                    Capabilities, Event, ExitedEventBody, InitializeRequestArguments,
                    MessageSeverity, Request, RttWindowOpenedArguments, TerminatedEventBody,
                },
                request_helpers::halt_core,
            },
            protocol::ProtocolAdapter,
        },
        peripherals::svd_variables::SvdCache,
        server::configuration::SessionConfig,
    },
    util::flash::build_loader,
};
use anyhow::{Context, anyhow};
use probe_rs::{
    Architecture, CoreStatus,
    config::Registry,
    flashing::{
        DownloadOptions, FileDownloadError, FlashError, FlashProgress, ProgressEvent,
        ProgressOperation,
    },
    probe::list::Lister,
};
use std::{
    cell::RefCell,
    fs,
    path::Path,
    thread,
    time::{Duration, UNIX_EPOCH},
};
use time::UtcOffset;

#[derive(Debug)]
/// The `DebuggerStatus` is used to control how the Debugger::debug_session() decides if it should respond to
/// DAP Client requests such as `Terminate`, `Disconnect`, and `Reset`, as well as how to respond to unrecoverable errors
/// during a debug session interacting with a target session.
pub(crate) enum DebugSessionStatus {
    Continue,
    Terminate,
    Restart(Request),
}

/// #Debugger Overview
/// The DAP Server may either be managed automatically by the development tool (typically an IDE or
/// editor the "DAP client") e.g. VSCode, or...
/// The DAP Server can optionally be run from the command line as a "server" process, and the
/// development tool can be configured to connect to it via a TCP connection.
/// - In this case, the management (start and stop) of the server process is the responsibility of the user. e.g.
///   - `probe-rs dap-server --port <IP port number> <other options>` : Uses TCP Sockets to the defined IP port number to service DAP requests.
pub struct Debugger {
    config: configuration::SessionConfig,

    /// UTC offset used for timestamps
    ///
    /// Getting the offset fails in multithreaded programs, so it's
    /// easier to determine it once and then save it.
    timestamp_offset: UtcOffset,

    // TODO: Store somewhere else
    /// Timestamp of the flashed binary
    binary_timestamp: Option<Duration>,

    /// Used to capture the `tracing` messages that are generated during the DAP sessions,
    /// to be ultimately forwarded to the DAP client's Debug Console, or failing that, stderr.
    pub(crate) debug_logger: DebugLogger,
}

impl Debugger {
    /// Create a new debugger instance
    pub fn new(
        timestamp_offset: UtcOffset,
        log_file: Option<&Path>,
    ) -> Result<Self, DebuggerError> {
        let mut debugger = Self {
            config: configuration::SessionConfig::default(),
            timestamp_offset,
            binary_timestamp: None,
            debug_logger: DebugLogger::new(log_file)?,
        };

        debugger
            .debug_logger
            .log_to_console("Starting probe-rs as a DAP Protocol server")?;

        Ok(debugger)
    }

    /// The logic of this function is as follows:
    /// - While we are waiting for DAP-Client, we have to continuously check in on the status of the probe.
    /// - Initially, while [`DebugAdapter::configuration_done`] = `false`, we do nothing.
    /// - Once [`DebugAdapter::configuration_done`] = `true`, we can start polling the probe for status, as follows:
    ///   - If the [`super::core_data::CoreData::last_known_status`] is `Halted(_)`, then we stop polling the Probe until the next DAP-Client request attempts an action
    ///   - If the `new_status` is an Err, then the probe is no longer available, and we  end the debugging session
    ///   - If the `new_status` is `Running`, then we have to poll on a regular basis, until the Probe stops for good reasons like breakpoints, or bad reasons like panics.
    pub(crate) fn process_next_request<P: ProtocolAdapter>(
        &mut self,
        session_data: &mut SessionData,
        debug_adapter: &mut DebugAdapter<P>,
    ) -> Result<DebugSessionStatus, DebuggerError> {
        self.debug_logger.flush_to_dap(debug_adapter)?;
        let Some(request) = debug_adapter.listen_for_request()? else {
            let _poll_span = tracing::trace_span!("Polling for core status").entered();
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
                    tracing::trace!(
                        "Retrieving data from the core, no delay required between iterations of polling the core."
                    );
                };
            }

            return Ok(DebugSessionStatus::Continue);
        };

        let _req_span = tracing::info_span!("Handling request", request = ?request).entered();

        // Poll ALL target cores for status, which includes synching status with the DAP client, and handling RTT data.
        let (core_statuses, _) = session_data.poll_cores(&self.config, debug_adapter)?;

        // Check if we have configured cores
        if core_statuses.is_empty() {
            if debug_adapter.configuration_is_done() {
                // We've passed `configuration_done` and still do not have at least one core configured.
                return Err(DebuggerError::Other(anyhow!(
                    "Cannot continue unless one target core configuration is defined."
                )));
            }

            // Keep processing "configuration" requests until we've passed `configuration_done` and have a valid `target_core`.
            return Ok(DebugSessionStatus::Continue);
        }

        // TODO: Currently, we only use `poll_cores()` results from the first core and need to expand to a multi-core implementation that understands which MS DAP requests are core specific.
        let core_id = 0;

        // Attach to the core. so that we have the handle available for processing the request.

        let Some(target_core_config) = self.config.core_configs.get(core_id) else {
            return Err(DebuggerError::Other(anyhow!(
                "No core configuration found for core id {}",
                core_id
            )));
        };

        let new_status = core_statuses[core_id]; // Checked above

        let mut target_core = session_data
            .attach_core(target_core_config.core_index)
            .context("Unable to connect to target core")?;

        // For some operations, we need to make sure the core isn't sleeping, by calling `Core::halt()`.
        // When we do this, we need to flag it (`unhalt_me = true`), and later call `Core::run()` again.
        // NOTE: The target will exit sleep mode as a result of this command.
        let mut unhalt_me = false;

        match request.command.as_ref() {
            "configurationDone"
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
                        Ok(_) => unhalt_me = true,
                        Err(error) => {
                            let err = DebuggerError::from(error);
                            debug_adapter.send_response::<()>(&request, Err(&err))?;
                            return Err(err);
                        }
                    }
                }
            }
            _ => {}
        }

        let mut debug_session = DebugSessionStatus::Continue;

        // Now we are ready to execute supported commands, or return an error if it isn't supported.
        let result = match request.command.as_ref() {
            "rttWindowOpened" => {
                if let Some(debugger_rtt_target) = target_core.core_data.rtt_connection.as_mut() {
                    let arguments: RttWindowOpenedArguments =
                        get_arguments(debug_adapter, &request)?;

                    if let Some(rtt_channel) = debugger_rtt_target
                        .debugger_rtt_channels
                        .iter_mut()
                        .find(|debugger_rtt_channel| {
                            debugger_rtt_channel.channel_number == arguments.channel_number
                        })
                    {
                        rtt_channel.has_client_window = arguments.window_is_open;
                    }

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
            "configurationDone" => debug_adapter.configuration_done(&mut target_core, &request),
            "threads" => debug_adapter.threads(&mut target_core, &request),
            "restart" => {
                if target_core.core.architecture() == Architecture::Riscv
                    && self.config.flashing_config.flashing_enabled
                {
                    debug_adapter.show_message(
                                MessageSeverity::Information,
                                "Re-flashing the target during on-session `restart` is not currently supported for RISC-V. Flashing will be disabled for the remainder of this session.",
                            );
                    self.config.flashing_config.flashing_enabled = false;
                }

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
                    Err(&DebuggerError::Other(anyhow!(
                        "Received request '{}', which is not supported or not implemented yet",
                        other_command
                    ))),
                )
            }
        };

        result.map_err(|e| DebuggerError::Other(e.context("Error executing request.")))?;

        if unhalt_me {
            if let Err(error) = target_core.core.run() {
                let error =
                    DebuggerError::Other(anyhow!(error).context("Failed to resume target."));
                debug_adapter.show_error_message(&error)?;
                return Err(error);
            }
        }

        Ok(debug_session)
    }

    /// `debug_session` is where the primary _debug processing_ for the DAP (Debug Adapter Protocol) adapter happens.
    /// All requests are interpreted, actions taken, and responses formulated here.
    /// This function is self contained and returns only status data to control what happens after the session completes.
    /// The [`DebugAdapter`] takes care of _implementing the DAP Base Protocol_ and _communicating with the DAP client_ and _probe_.
    pub(crate) fn debug_session<P: ProtocolAdapter + 'static>(
        &mut self,
        mut debug_adapter: DebugAdapter<P>,
        lister: &Lister,
    ) -> Result<(), DebuggerError> {
        let mut registry = Registry::from_builtin_families();
        // The DapClient startup process has a specific sequence.
        // Handle it here before starting a probe-rs session and looping through user generated requests.
        // Handling the initialize, and Attach/Launch requests here in this method,
        // before entering the iterative loop that processes requests through the process_request method.

        // Initialize request
        if self.handle_initialize(&mut debug_adapter).is_err() {
            // The request handler has already reported this error to the user.
            return Ok(());
        } else {
            self.debug_logger.flush_to_dap(&mut debug_adapter)?;
        }

        let launch_attach_request = loop {
            if let Some(request) = debug_adapter.listen_for_request()? {
                self.debug_logger.flush_to_dap(&mut debug_adapter)?;
                break request;
            }
        };

        // Process either the Launch or Attach request.
        let mut session_data = match self.handle_launch_attach(
            &mut registry,
            &launch_attach_request,
            &mut debug_adapter,
            lister,
        ) {
            Ok(session_data) => session_data,
            Err(error) => {
                debug_adapter.send_response::<()>(&launch_attach_request, Err(&error))?;
                return Ok(());
            }
        };
        self.debug_logger.flush_to_dap(&mut debug_adapter)?;

        if debug_adapter
            .send_event::<Event>("initialized", None)
            .is_err()
        {
            let error =
                DebuggerError::Other(anyhow!("Failed sending 'initialized' event to DAP Client"));

            debug_adapter.show_error_message(&error)?;

            return Err(error);
        }

        // Loop through remaining (user generated) requests and send to the [processs_request] method until either the client or some unexpected behaviour termintates the process.
        let error = loop {
            let debug_session_status =
                match self.process_next_request(&mut session_data, &mut debug_adapter) {
                    Ok(status) => status,
                    Err(error) => break error,
                };

            match debug_session_status {
                DebugSessionStatus::Continue => {
                    // All is good. We can process the next request.
                }
                DebugSessionStatus::Restart(request) => {
                    if let Err(error) =
                        self.restart(&mut debug_adapter, &mut session_data, &request)
                    {
                        debug_adapter.send_response::<()>(&request, Err(&error))?;
                        return Err(error);
                    }
                }
                DebugSessionStatus::Terminate => {
                    session_data.clean_up(&self.config)?;
                    return Ok(());
                }
            };
        };

        debug_adapter.show_message(
            MessageSeverity::Error,
            format!("Debug Adapter terminated unexpectedly with an error: {error:?}"),
        );
        debug_adapter.send_event("terminated", Some(TerminatedEventBody { restart: None }))?;
        debug_adapter.send_event("exited", Some(ExitedEventBody { exit_code: 1 }))?;
        // Keep the process alive for a bit, so that VSCode doesn't complain about broken pipes.
        for _loop_count in 0..10 {
            thread::sleep(Duration::from_millis(50));
        }

        Err(error)
    }

    /// Process launch or attach request
    #[tracing::instrument(skip_all, name = "Handle Launch/Attach Request")]
    fn handle_launch_attach<P: ProtocolAdapter + 'static>(
        &mut self,
        registry: &mut Registry,
        launch_attach_request: &Request,
        debug_adapter: &mut DebugAdapter<P>,
        lister: &Lister,
    ) -> Result<SessionData, DebuggerError> {
        let requested_target_session_type = match launch_attach_request.command.as_str() {
            "attach" => TargetSessionType::AttachRequest,
            "launch" => TargetSessionType::LaunchRequest,
            other => {
                return Err(DebuggerError::Other(anyhow!(
                    "Expected request 'launch' or 'attach', but received '{other}'"
                )));
            }
        };

        self.config = get_arguments(debug_adapter, launch_attach_request)?;

        self.config
            .validate_configuration_option_compatibility(requested_target_session_type)?;

        debug_adapter
            .set_console_log_level(self.config.console_log_level.unwrap_or(ConsoleLog::Console));

        self.config.validate_config_files()?;

        let mut session_data =
            SessionData::new(registry, lister, &mut self.config, self.timestamp_offset)?;

        debug_adapter.halt_after_reset = self.config.flashing_config.halt_after_reset;

        let Some(target_core_config) = self.config.core_configs.first() else {
            return Err(DebuggerError::Other(anyhow!(
                "Cannot continue unless one target core configuration is defined."
            )));
        };

        if self.config.flashing_config.flashing_enabled {
            let Some(path_to_elf) = &target_core_config.program_binary else {
                return Err(DebuggerError::Other(anyhow!(
                    "Please specify use the `program-binary` option in `launch.json` to specify an executable"
                )));
            };

            // Store timestamp of flashed binary
            self.binary_timestamp = get_file_timestamp(path_to_elf);

            Self::flash(
                &self.config,
                path_to_elf,
                debug_adapter,
                launch_attach_request,
                &mut session_data,
            )?;
        }

        // First, attach to the core
        let mut target_core = session_data.attach_core(target_core_config.core_index)?;

        // Immediately after attaching, halt the core, so that we can finish initalization without bumping into user code.
        // Depending on supplied `config`, the core will be restarted at the end of initialization in the `configuration_done` request.
        halt_core(&mut target_core.core)?;

        // Before we complete, load the (optional) CMSIS-SVD file and its variable cache.
        // Configure the [CorePeripherals].
        if let Some(svd_file) = &target_core_config.svd_file {
            target_core.core_data.core_peripherals =
                match SvdCache::new(svd_file, debug_adapter, launch_attach_request.seq) {
                    Ok(core_peripherals) => Some(core_peripherals),
                    Err(error) => {
                        // This is not a fatal error. We can continue the debug session without the SVD file.
                        tracing::warn!("{:?}", error);
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

        debug_adapter.send_response::<()>(launch_attach_request, Ok(None))?;

        Ok(session_data)
    }

    #[tracing::instrument(skip_all)]
    fn restart<P: ProtocolAdapter + 'static>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
        session_data: &mut SessionData,
        request: &Request,
    ) -> Result<(), DebuggerError> {
        let Some(target_core_config) = self.config.core_configs.first() else {
            return Err(DebuggerError::Other(anyhow!(
                "Cannot continue unless one target core configuration is defined."
            )));
        };

        if self.config.flashing_config.flashing_enabled {
            let Some(path_to_elf) = &target_core_config.program_binary else {
                return Err(DebuggerError::Other(anyhow!(
                    "Please specify use the `program-binary` option in `launch.json` to specify an executable"
                )));
            };

            if is_file_newer(&mut self.binary_timestamp, path_to_elf) {
                // If there is a new binary as part of a restart, there are some key things that
                // need to be 'reset' for things to work properly.
                session_data.load_debug_info_for_core(target_core_config)?;
                session_data
                    .attach_core(target_core_config.core_index)
                    .map(|mut target_core| target_core.recompute_breakpoints())??;

                Self::flash(
                    &self.config,
                    path_to_elf,
                    debug_adapter,
                    request,
                    session_data,
                )?;
            }
        }

        // First, attach to the core
        let mut target_core = session_data.attach_core(target_core_config.core_index)?;

        // Immediately after attaching, halt the core, so that we can finish restart logic without bumping into user code.
        halt_core(&mut target_core.core)?;

        // After completing optional flashing and other config, we can run the debug adapter's restart logic.
        debug_adapter
            .restart(&mut target_core, Some(request))
            .context("Failed to restart core")?;

        Ok(())
    }

    /// Flash the given binary, and report the progress to the
    /// debug adapter.
    // Note: This function consumes the 'debug_adapter', so all error reporting via that handle must be done before returning from this function.
    fn flash<P: ProtocolAdapter + 'static>(
        config: &SessionConfig,
        path_to_elf: &Path,
        debug_adapter: &mut DebugAdapter<P>,
        launch_attach_request: &Request,
        session_data: &mut SessionData,
    ) -> Result<(), DebuggerError> {
        debug_adapter.log_to_console(format!(
            "FLASHING: Starting write of {:?} to device memory",
            &path_to_elf
        ));
        let progress_id = debug_adapter
            .start_progress("Flashing device", Some(launch_attach_request.seq))
            .ok();

        let mut download_options = DownloadOptions::default();
        download_options.keep_unwritten_bytes = config.flashing_config.restore_unwritten_bytes;
        download_options.do_chip_erase = config.flashing_config.full_chip_erase;
        download_options.verify = config.flashing_config.verify_after_flashing;

        let ref_debug_adapter = RefCell::new(&mut *debug_adapter);

        struct ProgressState {
            total_page_size: u64,
            total_sector_size: u64,
            total_fill_size: u64,
            total_verify_size: u64,
            page_size_done: u64,
            sector_size_done: u64,
            fill_size_done: u64,
            verify_size_done: u64,
        }

        let progress_state = RefCell::new(ProgressState {
            total_page_size: 0,
            total_sector_size: 0,
            total_fill_size: 0,
            total_verify_size: 0,
            page_size_done: 0,
            verify_size_done: 0,
            sector_size_done: 0,
            fill_size_done: 0,
        });

        download_options.progress = progress_id.map(|id| {
            FlashProgress::new(move |event| {
                let mut flash_progress = progress_state.borrow_mut();
                let mut debug_adapter = ref_debug_adapter.borrow_mut();
                match event {
                    ProgressEvent::AddProgressBar {
                        operation,
                        total: Some(total),
                    } => match operation {
                        ProgressOperation::Fill => {
                            flash_progress.total_fill_size += total;
                            flash_progress.fill_size_done = 0;
                        }
                        ProgressOperation::Erase => {
                            flash_progress.total_sector_size += total;
                            flash_progress.sector_size_done = 0;
                        }
                        ProgressOperation::Program => {
                            flash_progress.total_page_size += total;
                            flash_progress.page_size_done = 0;
                        }
                        ProgressOperation::Verify => {
                            flash_progress.total_verify_size += total;
                            flash_progress.verify_size_done = 0;
                        }
                    },
                    ProgressEvent::Started(ProgressOperation::Fill) => {
                        debug_adapter
                            .update_progress(None, Some("Reading Old Pages"), id)
                            .ok();
                    }
                    ProgressEvent::Progress {
                        operation: ProgressOperation::Fill,
                        size,
                        ..
                    } => {
                        flash_progress.fill_size_done += size;
                        let progress = flash_progress.fill_size_done as f64
                            / flash_progress.total_fill_size as f64;

                        debug_adapter
                            .update_progress(Some(progress), Some("Reading Old Pages"), id)
                            .ok();
                    }
                    ProgressEvent::Failed(ProgressOperation::Fill) => {
                        debug_adapter
                            .update_progress(Some(1.0), Some("Reading Old Pages Failed!"), id)
                            .ok();
                    }
                    ProgressEvent::Finished(ProgressOperation::Fill) => {
                        debug_adapter
                            .update_progress(Some(1.0), Some("Reading Old Pages Complete!"), id)
                            .ok();
                    }
                    ProgressEvent::Started(ProgressOperation::Erase) => {
                        debug_adapter
                            .update_progress(None, Some("Erasing Sectors"), id)
                            .ok();
                    }
                    ProgressEvent::Progress {
                        operation: ProgressOperation::Erase,
                        size,
                        ..
                    } => {
                        flash_progress.sector_size_done += size;
                        let progress = flash_progress.sector_size_done as f64
                            / flash_progress.total_sector_size as f64;
                        debug_adapter
                            .update_progress(Some(progress), Some("Erasing Sectors"), id)
                            .ok();
                    }
                    ProgressEvent::Failed(ProgressOperation::Erase) => {
                        debug_adapter
                            .update_progress(Some(1.0), Some("Erasing Sectors Failed!"), id)
                            .ok();
                    }
                    ProgressEvent::Finished(ProgressOperation::Erase) => {
                        debug_adapter
                            .update_progress(Some(1.0), Some("Erasing Sectors Complete!"), id)
                            .ok();
                    }
                    ProgressEvent::Started(ProgressOperation::Program) => {
                        debug_adapter
                            .update_progress(None, Some("Programming Pages"), id)
                            .ok();
                    }
                    ProgressEvent::Progress {
                        operation: ProgressOperation::Program,
                        size,
                        ..
                    } => {
                        flash_progress.page_size_done += size;
                        let progress = flash_progress.page_size_done as f64
                            / flash_progress.total_page_size as f64;
                        debug_adapter
                            .update_progress(Some(progress), Some("Programming Pages"), id)
                            .ok();
                    }
                    ProgressEvent::Failed(ProgressOperation::Program) => {
                        debug_adapter
                            .update_progress(Some(1.0), Some("Flashing Pages Failed!"), id)
                            .ok();
                    }
                    ProgressEvent::Finished(ProgressOperation::Program) => {
                        debug_adapter
                            .update_progress(Some(1.0), Some("Flashing Pages Complete!"), id)
                            .ok();
                    }
                    ProgressEvent::Started(ProgressOperation::Verify) => {
                        debug_adapter
                            .update_progress(None, Some("Verifying"), id)
                            .ok();
                    }
                    ProgressEvent::Progress {
                        operation: ProgressOperation::Verify,
                        size,
                        ..
                    } => {
                        flash_progress.verify_size_done += size;
                        let progress = flash_progress.verify_size_done as f64
                            / flash_progress.total_verify_size as f64;

                        debug_adapter
                            .update_progress(Some(progress), Some("Verifying"), id)
                            .ok();
                    }
                    ProgressEvent::Failed(ProgressOperation::Verify) => {
                        debug_adapter
                            .update_progress(Some(1.0), Some("Verifying Failed!"), id)
                            .ok();
                    }
                    ProgressEvent::Finished(ProgressOperation::Verify) => {
                        debug_adapter
                            .update_progress(Some(1.0), Some("Verifying Complete!"), id)
                            .ok();
                    }
                    _ => {}
                }
            })
        });

        let result = match build_loader(
            &mut session_data.session,
            path_to_elf,
            config.flashing_config.format_options.clone(),
            None,
        ) {
            Ok(loader) => {
                let do_flashing = if config.flashing_config.verify_before_flashing {
                    match loader.verify(
                        &mut session_data.session,
                        download_options
                            .progress
                            .clone()
                            .unwrap_or_else(FlashProgress::empty),
                    ) {
                        Ok(_) => false,
                        Err(FlashError::Verify) => true,
                        Err(other) => {
                            return Err(DebuggerError::FileDownload(FileDownloadError::Flash(
                                other,
                            )));
                        }
                    }
                } else {
                    true
                };

                if do_flashing {
                    loader
                        .commit(&mut session_data.session, download_options)
                        .map_err(FileDownloadError::Flash)
                } else {
                    drop(download_options);
                    Ok(())
                }
            }
            Err(error) => {
                drop(download_options);
                Err(error)
            }
        };

        if let Some(id) = progress_id {
            let _ = debug_adapter.end_progress(id);
        }

        if result.is_ok() {
            debug_adapter.log_to_console(format!(
                "FLASHING: Completed write of {:?} to device memory",
                &path_to_elf
            ));
        }

        result.map_err(DebuggerError::FileDownload)
    }

    #[tracing::instrument(skip_all, name = "Handling initialize request")]
    fn handle_initialize<P: ProtocolAdapter>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
    ) -> Result<(), DebuggerError> {
        let initialize_request = expect_request(debug_adapter, "initialize")?;

        let initialize_arguments =
            get_arguments::<InitializeRequestArguments, _>(debug_adapter, &initialize_request)?;

        // Enable quirks specific to particular DAP clients...
        if let Some(client_id) = initialize_arguments.client_id {
            if client_id == "vscode" {
                tracing::info!(
                    "DAP client reports its 'ClientID' is 'vscode', enabling vscode_quirks."
                );
                debug_adapter.vscode_quirks = true;
            }
        }

        if !(initialize_arguments.columns_start_at_1.unwrap_or(true)
            && initialize_arguments.lines_start_at_1.unwrap_or(true))
        {
            debug_adapter.send_response::<()>(
                &initialize_request,
                Err(&DebuggerError::Other(anyhow!(
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
            support_terminate_debuggee: Some(true),
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
/// If the next request does *not* have the given command,
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
            Err(&DebuggerError::Other(anyhow!(
                "Initial command was '{}', expected '{}'",
                next_request.command,
                expected_command
            ))),
        )?;

        Err(DebuggerError::Other(anyhow!(
            "Initial command was '{}', expected '{}'",
            next_request.command,
            expected_command
        )))
    }
}

pub(crate) fn is_file_newer(
    saved_binary_timestamp: &mut Option<Duration>,
    path_to_elf: &Path,
) -> bool {
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

#[cfg(test)]
mod test {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use crate::cmd::dap_server::{
        DebuggerError,
        debug_adapter::{
            dap::{
                adapter::DebugAdapter,
                dap_types::{
                    Capabilities, DisassembleArguments, DisassembleResponseBody,
                    DisassembledInstruction, DisconnectArguments, ErrorResponseBody,
                    InitializeRequestArguments, Message, Request, Response, Source, Thread,
                    ThreadsResponseBody,
                },
            },
            protocol::ProtocolAdapter,
        },
        server::configuration::{ConsoleLog, CoreConfig, FlashingConfig, SessionConfig},
        test::TestLister,
    };
    use probe_rs::{
        architecture::arm::FullyQualifiedApAddress,
        integration::{FakeProbe, Operation},
        probe::{
            DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, ProbeFactory,
            list::Lister,
        },
    };
    use serde_json::json;
    use std::{
        collections::{BTreeMap, HashMap, VecDeque},
        fmt::Display,
        path::PathBuf,
    };
    use test_case::test_case;
    use time::UtcOffset;

    const TEST_CHIP_NAME: &str = "nRF52833_xxAA";

    #[derive(Debug)]
    struct MockProbeFactory;

    impl Display for MockProbeFactory {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Mocked Probe")
        }
    }

    impl ProbeFactory for MockProbeFactory {
        fn open(
            &self,
            _selector: &DebugProbeSelector,
        ) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
            todo!()
        }

        fn list_probes(&self) -> Vec<DebugProbeInfo> {
            todo!()
        }
    }

    /// Helper function to get the expected capabilities for the debugger
    ///
    /// `Capabilities::default()` is not const, so this can't just be a constant.
    fn expected_capabilites() -> Capabilities {
        Capabilities {
            support_suspend_debuggee: Some(true),
            supports_clipboard_context: Some(true),
            supports_completions_request: Some(true),
            supports_configuration_done_request: Some(true),
            supports_delayed_stack_trace_loading: Some(true),
            supports_disassemble_request: Some(true),
            supports_instruction_breakpoints: Some(true),
            supports_read_memory_request: Some(true),
            supports_write_memory_request: Some(true),
            supports_restart_request: Some(true),
            supports_set_variable: Some(true),
            supports_stepping_granularity: Some(true),
            support_terminate_debuggee: Some(true),

            ..Default::default()
        }
    }

    fn default_initialize_args() -> InitializeRequestArguments {
        InitializeRequestArguments {
            client_id: Some("mock_client".to_owned()),
            client_name: Some("Mock client for testing".to_owned()),
            adapter_id: "mock_adapter".to_owned(),
            columns_start_at_1: None,
            lines_start_at_1: None,
            locale: None,
            path_format: None,
            supports_args_can_be_interpreted_by_shell: None,
            supports_invalidated_event: None,
            supports_memory_event: None,
            supports_memory_references: None,
            supports_progress_reporting: None,
            supports_run_in_terminal_request: None,
            supports_start_debugging_request: None,
            supports_variable_paging: None,
            supports_variable_type: None,
        }
    }

    fn error_response_body(msg: &str) -> ErrorResponseBody {
        ErrorResponseBody {
            error: Some(error_message(msg)),
        }
    }

    fn error_message(msg: &str) -> Message {
        Message {
            format: "{response_message}".to_string(),
            id: 0,
            send_telemetry: Some(false),
            show_user: Some(true),
            url: Some("https://probe.rs/docs/tools/debugger/".to_string()),
            url_label: Some("Documentation".to_string()),
            variables: Some(BTreeMap::from([(
                "response_message".to_string(),
                msg.to_string(),
            )])),
        }
    }

    struct RequestBuilder<'r> {
        adapter: &'r mut MockProtocolAdapter,
    }

    impl<'r> RequestBuilder<'r> {
        fn with_arguments(self, arguments: impl serde::Serialize) -> Self {
            self.adapter.requests.back_mut().unwrap().arguments =
                Some(serde_json::to_value(arguments).unwrap());
            self
        }

        fn and_succesful_response(self) -> ResponseBuilder<'r> {
            let req = self.adapter.requests.back_mut().unwrap();

            let response = Response {
                command: req.command.clone(),
                request_seq: req.seq,
                seq: 0, // response sequence number is not checked
                success: true,
                message: None,
                body: None,
                type_: "response".to_string(),
            };

            self.adapter.expect_response(response)
        }

        fn and_error_response(self) -> ResponseBuilder<'r> {
            let req = self.adapter.requests.back_mut().unwrap();

            let response = Response {
                command: req.command.clone(),
                request_seq: req.seq,
                seq: 0, // response sequence number is not checked
                success: false,
                message: Some("cancelled".to_string()), // Currently always 'cancelled'
                body: None,
                type_: "response".to_string(),
            };

            self.adapter.expect_error_response(response)
        }
    }

    struct ResponseBuilder<'r> {
        adapter: &'r mut MockProtocolAdapter,
    }
    impl ResponseBuilder<'_> {
        fn with_body(self, body: impl serde::Serialize) {
            let resp = self.adapter.expected_responses.last_mut().unwrap();
            resp.body = Some(serde_json::to_value(body).unwrap());
        }
    }

    use super::Debugger;

    struct MockProtocolAdapter {
        requests: VecDeque<Request>,

        pending_requests: HashMap<i64, String>,

        sequence_number: i64,

        console_log_level: ConsoleLog,

        response_index: usize,
        expected_responses: Vec<Response>,

        event_index: usize,
        expected_events: Vec<(String, Option<serde_json::Value>)>,
    }

    impl MockProtocolAdapter {
        fn new() -> Self {
            Self {
                requests: VecDeque::new(),
                sequence_number: 0,
                pending_requests: HashMap::new(),
                console_log_level: ConsoleLog::Console,
                response_index: 0,
                expected_responses: Vec::new(),
                expected_events: Vec::new(),
                event_index: 0,
            }
        }

        fn add_request<'m>(&'m mut self, command: &str) -> RequestBuilder<'m> {
            let request = Request {
                arguments: None,
                command: command.to_string(),
                seq: self.sequence_number,
                type_: "request".to_string(),
            };

            self.pending_requests
                .insert(self.sequence_number, command.to_string());

            self.sequence_number += 1;

            self.requests.push_back(request);

            RequestBuilder { adapter: self }
        }

        fn expect_response(&mut self, response: Response) -> ResponseBuilder {
            assert!(
                response.success,
                "success field must be true for succesful response"
            );
            self.expected_responses.push(response);
            ResponseBuilder { adapter: self }
        }

        fn expect_error_response(&mut self, response: Response) -> ResponseBuilder {
            assert!(
                !response.success,
                "success field must be false for error response"
            );
            self.expected_responses.push(response);
            ResponseBuilder { adapter: self }
        }

        fn expect_event(&mut self, event_type: &str, event_body: Option<impl serde::Serialize>) {
            let event_body = event_body.map(|s| serde_json::to_value(s).unwrap());

            self.expected_events
                .push((event_type.to_owned(), event_body));
        }

        fn expect_output_event(&mut self, msg: &str) {
            self.expect_event(
                "output",
                Some(json!({
                    "category": "console",
                    "group": "probe-rs-debug",
                    "output":  msg
                })),
            );
        }
    }

    impl ProtocolAdapter for MockProtocolAdapter {
        fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>> {
            let next_request = self
                .requests
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("No more responses to listen for."))?;

            Ok(Some(next_request))
        }

        fn send_event<S: serde::Serialize>(
            &mut self,
            event_type: &str,
            event_body: Option<S>,
        ) -> anyhow::Result<()> {
            let event_body = event_body.map(|s| serde_json::to_value(s).unwrap());

            if self.event_index >= self.expected_events.len() {
                panic!(
                    "No more events expected, but got event_type={:?}, event_body={:?}",
                    event_type, event_body
                );
            }

            let (expected_event_type, expected_event_body) =
                &self.expected_events[self.event_index];

            pretty_assertions::assert_eq!(
                (event_type, &event_body),
                (expected_event_type.as_str(), expected_event_body)
            );

            self.event_index += 1;

            Ok(())
        }

        fn set_console_log_level(
            &mut self,
            _log_level: crate::cmd::dap_server::server::configuration::ConsoleLog,
        ) {
        }

        fn console_log_level(&self) -> crate::cmd::dap_server::server::configuration::ConsoleLog {
            self.console_log_level
        }

        fn send_raw_response(&mut self, response: &Response) -> anyhow::Result<()> {
            if self.response_index >= self.expected_responses.len() {
                panic!("No more responses expected, but got {response:?}");
            }

            let expected_response = &self.expected_responses[self.response_index];

            // We don't check the sequence number of the response

            let response = Response {
                seq: expected_response.seq,
                ..response.clone()
            };

            pretty_assertions::assert_eq!(&response, expected_response);

            self.response_index += 1;

            Ok(())
        }

        fn remove_pending_request(&mut self, request_seq: i64) -> Option<String> {
            self.pending_requests.remove(&request_seq)
        }
    }

    fn initialized_protocol_adapter() -> MockProtocolAdapter {
        let mut protocol_adapter = MockProtocolAdapter::new();

        protocol_adapter
            .add_request("initialize")
            .with_arguments(default_initialize_args())
            .and_succesful_response()
            .with_body(expected_capabilites());

        protocol_adapter.expect_output_event("probe-rs-debug: Log output for \"probe_rs=warn\" will be written to the Debug Console.\n");
        protocol_adapter
            .expect_output_event("probe-rs-debug: Starting probe-rs as a DAP Protocol server\n");

        protocol_adapter
    }

    fn program_binary() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../probe-rs-debug/tests/debug-unwind-tests/nRF52833_xxAA_full_unwind.elf")
    }

    fn valid_session_config() -> SessionConfig {
        SessionConfig {
            chip: Some(TEST_CHIP_NAME.to_owned()),
            core_configs: vec![CoreConfig {
                core_index: 0,
                program_binary: Some(program_binary()),
                ..CoreConfig::default()
            }],
            ..SessionConfig::default()
        }
    }

    fn launched_protocol_adapter() -> MockProtocolAdapter {
        let mut protocol_adapter = initialized_protocol_adapter();

        let launch_args = valid_session_config();
        protocol_adapter
            .add_request("launch")
            .with_arguments(launch_args)
            .and_succesful_response();

        protocol_adapter.expect_event("initialized", None::<u32>);

        protocol_adapter
    }

    fn disconnect_protocol_adapter(protocol_adapter: &mut MockProtocolAdapter) {
        protocol_adapter
            .add_request("disconnect")
            .with_arguments(DisconnectArguments {
                restart: Some(false),
                suspend_debuggee: Some(false),
                terminate_debuggee: Some(false),
            })
            .and_succesful_response();
    }

    fn fake_probe() -> (DebugProbeInfo, FakeProbe) {
        let probe_info = DebugProbeInfo::new(
            "Mock probe",
            0x12,
            0x23,
            Some("mock_serial".to_owned()),
            &MockProbeFactory,
            None,
        );

        let fake_probe = FakeProbe::with_mocked_core_and_binary(program_binary().as_path());

        // Indicate that the core is unlocked
        fake_probe.expect_operation(Operation::ReadRawApRegister {
            ap: FullyQualifiedApAddress::v1_with_default_dp(1),
            address: 0xC,
            result: 1,
        });

        (probe_info, fake_probe)
    }

    fn execute_test(
        protocol_adapter: MockProtocolAdapter,
        with_probe: bool,
    ) -> Result<(), DebuggerError> {
        let debug_adapter = DebugAdapter::new(protocol_adapter);

        let lister = TestLister::new();
        if with_probe {
            lister.probes.borrow_mut().push(fake_probe());
        }
        let lister = Lister::with_lister(Box::new(lister));

        let mut debugger = Debugger::new(UtcOffset::UTC, None)?;
        debugger.debug_session(debug_adapter, &lister)
    }

    #[test]
    fn test_initalize_request() {
        let protocol_adapter = initialized_protocol_adapter();

        // TODO: Check proper return value
        execute_test(protocol_adapter, false).unwrap_err();
    }

    #[test]
    fn test_launch_no_probes() {
        let mut protocol_adapter = initialized_protocol_adapter();

        let expected_error = "No connected probes were found.";
        protocol_adapter.expect_output_event(&format!("{expected_error}\n"));

        protocol_adapter
            .add_request("launch")
            .with_arguments(SessionConfig::default())
            .and_error_response()
            .with_body(error_response_body(expected_error));

        execute_test(protocol_adapter, false).unwrap();
    }

    #[test]
    fn test_launch_and_terminate() {
        let mut protocol_adapter = launched_protocol_adapter();

        disconnect_protocol_adapter(&mut protocol_adapter);

        execute_test(protocol_adapter, true).unwrap();
    }

    #[test]
    fn launch_with_config_error() {
        let mut protocol_adapter = initialized_protocol_adapter();

        let invalid_launch_args = SessionConfig {
            chip: Some(TEST_CHIP_NAME.to_owned()),
            core_configs: vec![CoreConfig {
                core_index: 0,
                ..CoreConfig::default()
            }],
            ..SessionConfig::default()
        };

        let expected_error = "Please use the `program-binary` option to specify an executable for this target core. Other(Missing value for file.)";
        protocol_adapter.expect_output_event(&format!("{expected_error}\n"));

        protocol_adapter
            .add_request("launch")
            .with_arguments(invalid_launch_args)
            .and_error_response()
            .with_body(error_response_body(expected_error));

        execute_test(protocol_adapter, true).unwrap();
    }

    #[test]
    fn wrong_request_after_init() {
        let mut protocol_adapter = initialized_protocol_adapter();

        let expected_error = "Expected request 'launch' or 'attach', but received 'threads'";
        protocol_adapter.expect_output_event(&format!("{expected_error}\n"));

        protocol_adapter
            .add_request("threads")
            .and_error_response()
            .with_body(error_response_body(expected_error));

        execute_test(protocol_adapter, true).unwrap();
    }

    #[test]
    fn attach_request() {
        let mut protocol_adapter = initialized_protocol_adapter();

        let attach_args = valid_session_config();
        protocol_adapter
            .add_request("attach")
            .with_arguments(attach_args)
            .and_succesful_response();

        protocol_adapter.expect_event("initialized", None::<u32>);

        disconnect_protocol_adapter(&mut protocol_adapter);

        execute_test(protocol_adapter, true).unwrap();
    }

    #[test]
    fn attach_with_flashing() {
        let mut protocol_adapter = initialized_protocol_adapter();

        let attach_args = SessionConfig {
            flashing_config: FlashingConfig {
                flashing_enabled: true,
                halt_after_reset: true,
                ..Default::default()
            },
            ..valid_session_config()
        };

        let expected_error = "Please do not use any of the `flashing_enabled`, `reset_after_flashing`, halt_after_reset`, `full_chip_erase`, or `restore_unwritten_bytes` options when using `attach` request type.";
        protocol_adapter.expect_output_event(&format!("{expected_error}\n"));

        protocol_adapter
            .add_request("attach")
            .with_arguments(attach_args)
            .and_error_response()
            .with_body(error_response_body(expected_error));

        execute_test(protocol_adapter, true).unwrap();
    }

    #[test]
    fn launch_and_threads() {
        let mut protocol_adapter = launched_protocol_adapter();

        protocol_adapter
            .add_request("configurationDone")
            .and_succesful_response();

        protocol_adapter
            .add_request("threads")
            .and_succesful_response()
            .with_body(ThreadsResponseBody {
                threads: vec![Thread {
                    id: 0,
                    name: format!("0-{TEST_CHIP_NAME}"),
                }],
            });

        disconnect_protocol_adapter(&mut protocol_adapter);

        execute_test(protocol_adapter, true).unwrap();
    }

    #[test_case(0; "instructions before and not including the ref address, multiple locations")]
    #[test_case(1; "instructions including the ref address, location cloned from earlier line")]
    #[test_case(2; "instructions after and not including the ref address")]
    #[test_case(3; "negative byte offset of exactly one instruction (aligned)")]
    #[test_case(4; "positive byte offset that lands in the middle of an instruction (unaligned)")]
    fn disassemble(test_case: usize) {
        #[rustfmt::skip]
        mod config {
            use std::collections::HashMap;

            type TestInstruction = (&'static str, &'static str, &'static str);
            const TEST_INSTRUCTIONS: [TestInstruction; 10] = [
                // address, instruction, instruction_bytes
                ("0x00000772", "b  #0x7a8", "19 E0"),         // 32 bit Thumb-v2 instruction
                ("0x00000774", "ldr  r0, [sp, #4]", "01 98"), // 16 bit Thumb-v2 instruction
                ("0x00000776", "mov.w  r1, #0x55555555", "4F F0 55 31"),
                ("0x0000077A", "and.w  r1, r1, r0, lsr #1", "01 EA 50 01"),
                ("0x0000077E", "subs  r0, r0, r1", "40 1A"),
                ("0x00000780", "mov.w  r1, #0x33333333", "4F F0 33 31"),
                ("0x00000784", "and.w  r1, r1, r0, lsr #2", "01 EA 90 01"),
                ("0x00000788", "bic  r0, r0, #0xcccccccc", "20 F0 CC 30"),
                ("0x0000078C", "add  r0, r1", "08 44"),
                ("0x0000078E", "add.w  r0, r0, r0, lsr #4", "00 EB 10 10"),
            ];

            type TestLocation = (i64, i64, &'static str, &'static str, &'static str);
            const TEST_LOCATIONS: [TestLocation; 3] = [
                // line, column, name, path, presentation_hint
                (115, 5, "<unavailable>: ub_checks.rs", "/rustc/7f2fc33da6633f5a764ddc263c769b6b2873d167/library/core/src/ub_checks.rs", "deemphasize"),
                (0, 5, "<unavailable>: ub_checks.rs", "/rustc/7f2fc33da6633f5a764ddc263c769b6b2873d167/library/core/src/ub_checks.rs", "deemphasize"),
                (1244, 5, "<unavailable>: mod.rs", "/rustc/7f2fc33da6633f5a764ddc263c769b6b2873d167/library/core/src/num/mod.rs", "deemphasize"),
            ];

            type TestCase = (&'static str, i64, i64, i64, &'static [TestInstruction], HashMap<&'static str, &'static TestLocation>);
            pub(super) fn test_cases() -> [TestCase; 5] {[
                // memory reference, byte offset, instruction_offset, instruction_count, expected instructions,
                //    hash from instruction addresses to expected locations:

                // Test Case: instructions before and not including the ref address, multiple locations
                ("0x00000788", 0, -7, 6, &TEST_INSTRUCTIONS[0..6],
                    HashMap::from([("0x00000772", &TEST_LOCATIONS[0]), ("0x00000774", &TEST_LOCATIONS[1]), ("0x0000077A", &TEST_LOCATIONS[2])])),

                // Test Case: instructions including the ref address, location cloned from earlier line
                ("0x00000788", 0, -3, 6, &TEST_INSTRUCTIONS[4..10],
                    HashMap::from([("0x0000077E", &TEST_LOCATIONS[2])])),

                // Test Case: instructions after and not including the ref address
                ("0x00000772", 0, 3, 6, &TEST_INSTRUCTIONS[3..9],
                    HashMap::from([("0x0000077A", &TEST_LOCATIONS[2])])),

                // Test Case: negative byte offset of exactly one instruction (aligned)
                ("0x00000772", -4, 3, 6, &TEST_INSTRUCTIONS[2..8],
                    HashMap::from([("0x00000776", &TEST_LOCATIONS[1]), ("0x0000077A", &TEST_LOCATIONS[2])])),

                // Test Case: positive byte offset that lands in the middle of an instruction (unaligned):
                //            automatic instruction alignment and defensive ref address matching
                ("0x00000776", 6, 0, 6, &TEST_INSTRUCTIONS[4..10],
                    HashMap::from([("0x0000077E", &TEST_LOCATIONS[2])])),
            ]}
        }

        let mut protocol_adapter = launched_protocol_adapter();

        protocol_adapter
            .add_request("configurationDone")
            .and_succesful_response();

        let default_instruction_fields = DisassembledInstruction {
            address: "".to_string(),
            column: None,
            end_column: None,
            end_line: None,
            instruction: "".to_string(),
            instruction_bytes: None,
            line: None,
            location: None,
            symbol: None,
        };

        let default_source_fields = Source {
            adapter_data: None,
            checksums: None,
            name: None,
            origin: None,
            path: None,
            presentation_hint: None,
            source_reference: None,
            sources: None,
        };

        let (mem, off, inst_off, inst_cnt, test_instrs, test_locs) =
            &config::test_cases()[test_case];

        protocol_adapter
            .add_request("disassemble")
            .with_arguments(DisassembleArguments {
                memory_reference: mem.to_string(),
                offset: Some(*off),
                instruction_offset: Some(*inst_off),
                instruction_count: *inst_cnt,
                resolve_symbols: None,
            })
            .and_succesful_response()
            .with_body(DisassembleResponseBody {
                instructions: test_instrs
                    .iter()
                    .map(|(address, instruction, instruction_bytes)| {
                        let mut instruction = DisassembledInstruction {
                            address: (*address).to_owned(),
                            instruction: (*instruction).to_owned(),
                            instruction_bytes: Some((*instruction_bytes).to_owned()),
                            ..default_instruction_fields.clone()
                        };
                        if let Some(&(line, column, name, path, hint)) = test_locs.get(address) {
                            instruction.line = if *line == 0 { None } else { Some(*line) };
                            instruction.column = Some(*column);
                            instruction.location = Some(Source {
                                name: Some(name.to_string()),
                                path: Some(path.to_string()),
                                presentation_hint: Some(hint.to_string()),
                                ..default_source_fields.clone()
                            })
                        }
                        instruction
                    })
                    .collect(),
            });

        disconnect_protocol_adapter(&mut protocol_adapter);

        execute_test(protocol_adapter, true).unwrap();
    }
}
