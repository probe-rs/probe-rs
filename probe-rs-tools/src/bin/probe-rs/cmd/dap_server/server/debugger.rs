use super::{
    configuration::{self, ConsoleLog},
    logger::DebugLogger,
    session_data::SessionData,
    startup::{TargetSessionType, get_file_timestamp},
    uploaded_files::UploadedFiles,
};
use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::dap::{
        adapter::{DebugAdapter, get_arguments},
        dap_types::{
            Capabilities, DisconnectResponse, Event, ExitedEventBody, InitializeRequestArguments,
            MessageSeverity, Request, TerminatedEventBody,
        },
    },
    debug_adapter::protocol::RequestSummary,
    server::configuration::SessionConfig,
};
use anyhow::{Context, anyhow};
use probe_rs::CoreStatus;
use probe_rs_rpc::flash::{Operation, ProgressEvent as WireProgressEvent};
use probe_rs_rpc_client::{ResolvedUpload, RpcClient};
use std::{collections::HashMap, path::Path, time::Duration};
use time::UtcOffset;

fn dap_capabilities() -> Capabilities {
    Capabilities {
        supports_configuration_done_request: Some(true),
        supports_restart_request: Some(true),
        support_suspend_debuggee: Some(true),
        support_terminate_debuggee: Some(true),
        supports_evaluate_for_hovers: Some(true),
        // stackTrace serves the halt-time display cache; it does not
        // perform an on-demand unwind for startFrame/levels requests.
        supports_delayed_stack_trace_loading: Some(false),
        supports_read_memory_request: Some(true),
        supports_write_memory_request: Some(true),
        supports_set_variable: Some(true),
        supports_disassemble_request: Some(true),
        supports_instruction_breakpoints: Some(true),
        supports_stepping_granularity: Some(true),
        supports_completions_request: Some(true),
        // ANSI output is emitted only when the client also opts in.
        supports_ansi_styling: Some(true),
        ..Default::default()
    }
}

#[derive(Debug)]
/// Controls how `debug_session` responds to client `Terminate`/`Disconnect`/`Reset`
/// requests and to unrecoverable errors during a target session.
pub(crate) enum DebugSessionStatus {
    /// Continue handling requests after a specified delay.
    Continue(Duration),
    Terminate,
    Restart(Request),
}

/// A failure that occurs while the server handles a request.
///
/// The `?` operator makes a [`RequestFailure::Request`]. Use
/// [`RequestFailure::Session`] when the failed operation is necessary for the
/// session, and not only for the request.
enum RequestFailure {
    /// The request failed. The client receives an error response, and the
    /// session continues.
    Request(DebuggerError),
    /// The session cannot continue.
    Session(DebuggerError),
}

impl RequestFailure {
    fn error(&self) -> &DebuggerError {
        match self {
            Self::Request(error) | Self::Session(error) => error,
        }
    }
}

impl From<DebuggerError> for RequestFailure {
    fn from(error: DebuggerError) -> Self {
        Self::Request(error)
    }
}

impl From<anyhow::Error> for RequestFailure {
    fn from(error: anyhow::Error) -> Self {
        Self::Request(DebuggerError::Other(error))
    }
}

/// Top-level DAP server driver. May be managed by an IDE/editor (e.g. VSCode)
/// or run standalone over TCP via `probe-rs dap-server --port <port>`.
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

    // TODO: Store somewhere else
    /// Timestamp of the SVD file that the server parsed last
    svd_timestamp: Option<Duration>,

    /// Used to capture the `tracing` messages that are generated during the DAP sessions,
    /// to be ultimately forwarded to the DAP client's Debug Console, or failing that, stderr.
    pub(crate) debug_logger: DebugLogger,

    /// Session-scoped temporary directory holding files uploaded by the DAP client when running
    /// in `remote_server_mode` (program binary, SVD file, chip description).
    ///
    /// Created at the start of each remote-mode session (in [`Self::handle_launch_attach`]) and dropped at the
    /// end of that session (in [`Self::debug_session_impl`]); `None` in local mode and between
    /// sessions in TCP multi-session mode.
    uploaded_files: Option<UploadedFiles>,

    /// Optional RPC session already attached by the CLI (for example after
    /// `cli::flash`). When set, [`SessionData::new_rpc_backed`] reuses it
    /// instead of calling `probe/attach` again.
    pub(crate) preattached_session: Option<probe_rs_rpc_client::SessionInterface>,
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
            svd_timestamp: None,
            debug_logger: DebugLogger::new(log_file)?,
            uploaded_files: None,
            preattached_session: None,
        };

        debugger
            .debug_logger
            .log_to_console("Starting probe-rs as a DAP Protocol server")?;

        Ok(debugger)
    }

    /// The logic of this function is as follows:
    /// - While we are waiting for DAP-Client, periodically query core status through the RPC backend.
    /// - Initially, while [`DebugAdapter::configuration_done`] = `false`, we do nothing.
    /// - Once [`DebugAdapter::configuration_done`] = `true`, status polling proceeds as follows:
    ///   - If the [`super::core_data::CoreData::last_known_status`] is `Halted(_)`, then we stop sending status RPCs until the next DAP-Client request attempts an action
    ///   - If the `new_status` is an Err, then the probe is no longer available, and we  end the debugging session
    ///   - If the `new_status` is `Running`, then we poll on a regular basis until the target stops for good reasons like breakpoints, or bad reasons like panics.
    pub(crate) async fn process_next_request(
        &mut self,
        session_data: &mut SessionData,
        debug_adapter: &mut DebugAdapter,
    ) -> Result<DebugSessionStatus, DebuggerError> {
        self.debug_logger.flush_to_dap(debug_adapter)?;

        if let Some(request) = debug_adapter.listen_for_request()? {
            self.handle_request(session_data, debug_adapter, request)
                .await
        } else {
            self.no_request_poll(session_data, debug_adapter).await
        }
    }

    /// Handles a request and sends an error response if necessary.
    ///
    /// A request that fails does not end the session. A target that becomes
    /// unreachable is caught by the next poll of the cores.
    async fn handle_request(
        &mut self,
        session_data: &mut SessionData,
        debug_adapter: &mut DebugAdapter,
        request: Request,
    ) -> Result<DebugSessionStatus, DebuggerError> {
        let failure = match self
            .handle_request_impl(session_data, debug_adapter, &request)
            .await
        {
            Ok(status) => return Ok(status),
            Err(failure) => failure,
        };

        // In case the error response fails, we still want to return the result.
        if let Err(response_error) = debug_adapter.send_error_response(&request, failure.error()) {
            tracing::warn!("Failed to send error response: {response_error}");
        }

        match failure {
            // A client that repeats a failing request must not spin the server.
            RequestFailure::Request(_) => {
                Ok(DebugSessionStatus::Continue(Duration::from_millis(50)))
            }
            RequestFailure::Session(error) => Err(error),
        }
    }

    async fn handle_request_impl(
        &mut self,
        session_data: &mut SessionData,
        debug_adapter: &mut DebugAdapter,
        request: &Request,
    ) -> Result<DebugSessionStatus, RequestFailure> {
        let _req_span =
            tracing::info_span!("Handling request", request = ?RequestSummary(request)).entered();

        // Poll ALL target cores for status, which includes synching status with the DAP client, and handling RTT data.
        session_data
            .poll_cores(&self.config, debug_adapter)
            .await
            .map_err(RequestFailure::Session)?;

        // Check if we have configured cores
        if session_data.core_data.is_empty() {
            if debug_adapter.configuration_is_done() {
                // We've passed `configuration_done` and still do not have at least one core configured.
                return Err(RequestFailure::Session(DebuggerError::Other(anyhow!(
                    "Cannot continue unless one target core configuration is defined."
                ))));
            }

            // Keep processing "configuration" requests until we've passed `configuration_done` and have a valid `target_core`.
            return Ok(DebugSessionStatus::Continue(Duration::ZERO));
        }

        // TODO: Currently, we only use `poll_cores()` results from the first core and need to expand
        // to a multi-core implementation that understands which MS DAP requests are core specific.
        let core_id = 0;

        let Some(target_core_config) = self.config.core_configs.get(core_id) else {
            return Err(RequestFailure::Session(DebuggerError::Other(anyhow!(
                "No core configuration found for core id {core_id}"
            ))));
        };
        let core_index = target_core_config.core_index;

        // Some operations require a sleeping core to be halted through the
        // RPC backend. Track that temporary halt so the backend can resume it
        // after the request.
        // NOTE: The target will exit sleep mode as a result of this command.
        let mut unhalt_me = false;
        {
            let new_status = session_data
                .core_data_opt(core_index)
                .map(|cd| cd.last_known_status)
                .unwrap_or(CoreStatus::Unknown);
            if matches!(
                request.command.as_ref(),
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
                    | "disassemble"
            ) && new_status == CoreStatus::Sleeping
            {
                session_data
                    .backend
                    .halt(core_index, Duration::from_millis(100))
                    .await
                    .map_err(DebuggerError::from)?;
                unhalt_me = true;
            }
        }

        let mut debug_session = DebugSessionStatus::Continue(Duration::ZERO);
        match request.command.as_ref() {
            "setBreakpoints" => {
                debug_adapter
                    .set_breakpoints(session_data, core_index, request)
                    .await?;
            }
            "setInstructionBreakpoints" => {
                debug_adapter
                    .set_instruction_breakpoints(session_data, core_index, request)
                    .await?;
            }
            "readMemory" => {
                debug_adapter
                    .read_memory(session_data, core_index, request)
                    .await?;
            }
            "writeMemory" => {
                debug_adapter
                    .write_memory(session_data, core_index, request)
                    .await?;
            }
            "pause" => {
                debug_adapter
                    .pause(session_data, core_index, request)
                    .await?;
            }
            "scopes" => {
                debug_adapter
                    .scopes(session_data, core_index, request)
                    .await?;
            }
            "variables" => {
                debug_adapter
                    .variables(session_data, core_index, request)
                    .await?;
            }
            "evaluate" => {
                debug_adapter
                    .evaluate(session_data, core_index, request)
                    .await?;
            }
            "stackTrace" => {
                debug_adapter
                    .stack_trace(session_data, core_index, request)
                    .await?;
            }
            "next" => {
                debug_adapter
                    .next(session_data, core_index, request)
                    .await?;
            }
            "stepIn" => {
                debug_adapter
                    .step_in(session_data, core_index, request)
                    .await?;
            }
            "stepOut" => {
                debug_adapter
                    .step_out(session_data, core_index, request)
                    .await?;
            }
            "setVariable" => {
                debug_adapter
                    .set_variable(session_data, core_index, request)
                    .await?;
            }
            "disassemble" => {
                debug_adapter
                    .disassemble(session_data, core_index, request)
                    .await?;
            }
            "configurationDone" => {
                debug_adapter
                    .configuration_done(session_data, core_index, request)
                    .await?;
            }
            "threads" => {
                debug_adapter
                    .threads(session_data, core_index, request)
                    .await?;
            }
            "completions" => {
                debug_adapter
                    .completions(session_data, core_index, request)
                    .await?;
            }
            "rttWindowOpened" => {
                debug_adapter
                    .rtt_window_opened(session_data, core_index, request)
                    .await?;
            }
            "continue" => {
                debug_adapter
                    .r#continue(session_data, core_index, request)
                    .await?;
            }

            "disconnect" => {
                debug_adapter
                    .disconnect(session_data, core_index, request)
                    .await?;
                debug_session = DebugSessionStatus::Terminate;
            }
            "restart" => {
                session_data
                    .backend
                    .halt(core_index, Duration::from_millis(500))
                    .await
                    .context("Failed to halt core")?;
                debug_session = DebugSessionStatus::Restart(request.clone());
            }
            _ => {
                let unimplemented_command = request.command.as_str();
                debug_adapter.send_response::<()>(
                    request,
                    Err(&DebuggerError::Other(anyhow!(
                        "Received request '{unimplemented_command}', which is not supported or not implemented yet"
                    ))),
                ).context("Error executing request.")?;
            }
        };

        if unhalt_me && let Err(error) = session_data.backend.run(core_index).await {
            let error = DebuggerError::Other(anyhow!(error).context("Failed to resume target."));
            debug_adapter.show_error_message(&error)?;
            return Err(error.into());
        }

        Ok(debug_session)
    }

    async fn no_request_poll(
        &mut self,
        session_data: &mut SessionData,
        debug_adapter: &mut DebugAdapter,
    ) -> Result<DebugSessionStatus, DebuggerError> {
        let _poll_span = tracing::trace_span!("Polling for core status").entered();
        let delay;

        // Poll ALL target cores for status, which includes synching status with the DAP client, and handling RTT data.
        // We do this even if the cores may be halted, as we need to handle RTT data from cores the debugger does not control.
        let suggest_delay_required = session_data.poll_cores(&self.config, debug_adapter).await?;

        if debug_adapter.all_cores_halted {
            // Medium delay to reduce fast looping costs.
            delay = Duration::from_millis(100);

            // Once all cores are halted, then we can skip polling the core for status, and just wait for the next DAP Client request.
            tracing::trace!(
                "Sleeping (all cores are halted) for {delay:?} to reduce polling overheads."
            );
        } else {
            // If there are no requests from the DAP Client, and there was no
            // RTT data in the last poll, then we can sleep for a short period of time to reduce CPU usage.
            if debug_adapter.configuration_is_done() && suggest_delay_required {
                // Small delay to reduce fast looping costs.
                delay = Duration::from_millis(50);

                tracing::trace!(
                    "Sleeping (core is running) for {delay:?} to reduce polling overheads."
                );
            } else {
                delay = Duration::ZERO;

                tracing::trace!(
                    "Retrieving data from the core, no delay required between iterations of polling the core."
                );
            };
        }

        Ok(DebugSessionStatus::Continue(delay))
    }

    /// RPC entry point for the DAP server. Drives the session against an
    /// [`crate::cmd::dap_server::backend::rpc::RpcBackend`] wired up around the
    /// provided [`RpcClient`] (and its ambient tokio runtime). The chip registry
    /// of the server that `client` talks to supplies the target descriptions.
    pub(crate) async fn debug_session_rpc(
        &mut self,
        client: &RpcClient,
        debug_adapter: DebugAdapter,
    ) -> Result<(), DebuggerError> {
        let timestamp_offset = self.timestamp_offset;
        let result = self
            .debug_session_impl(debug_adapter, client, timestamp_offset)
            .await;
        // Drop the session-scoped temporary directory holding any client-uploaded
        // files (program binary, SVD, chip description). Done at session end rather
        // than at [`Debugger`] drop so that, in TCP multi-session mode, one client's
        // uploaded firmware does not linger on disk after they disconnect (and is
        // not visible to the next client that connects).
        self.uploaded_files = None;
        result
    }

    /// Generic driver for a DAP session.
    async fn debug_session_impl(
        &mut self,
        mut debug_adapter: DebugAdapter,
        client: &RpcClient,
        timestamp_offset: UtcOffset,
    ) -> Result<(), DebuggerError> {
        // Handle the initialize + attach/launch sequence before entering the
        // request loop.

        // Initialize request
        if self.handle_initialize(&mut debug_adapter).is_err() {
            // The request handler has already reported this error to the user.
            return Ok(());
        }

        let Some(mut session_data) = self
            .start_session(&mut debug_adapter, client, timestamp_offset)
            .await?
        else {
            // We got no error, but no SessionData, either
            return Ok(());
        };

        if debug_adapter
            .send_event::<Event>("initialized", None)
            .is_err()
        {
            let error =
                DebuggerError::Other(anyhow!("Failed sending 'initialized' event to DAP Client"));

            debug_adapter.show_error_message(&error)?;

            return Err(error);
        }

        // Loop through user-generated requests until the client or an error
        // terminates the session.
        let error = loop {
            let debug_session_status = match self
                .process_next_request(&mut session_data, &mut debug_adapter)
                .await
            {
                Ok(status) => status,
                Err(error) => break error,
            };

            match debug_session_status {
                DebugSessionStatus::Continue(delay) => {
                    // All is good. We can process the next request.
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                }
                DebugSessionStatus::Restart(request) => {
                    if let Err(error) = self
                        .restart(&mut debug_adapter, &mut session_data, &request)
                        .await
                    {
                        // Report the failed restart, then end the session
                        // through the common path, so that the client also
                        // receives the `terminated` and `exited` events. A
                        // client that receives neither shows a session that
                        // does not respond to any action.
                        let _ = debug_adapter.send_error_response(&request, &error);
                        break error;
                    }
                }
                DebugSessionStatus::Terminate => {
                    session_data.clean_up(&self.config).await?;
                    return Ok(());
                }
            };
        };

        tracing::error!("The debug session ends with an error: {error:?}");
        debug_adapter.show_message(
            MessageSeverity::Error,
            format!("Debug Adapter terminated unexpectedly with an error: {error:?}"),
        );
        debug_adapter.send_event("terminated", Some(TerminatedEventBody { restart: None }))?;
        debug_adapter.send_event("exited", Some(ExitedEventBody { exit_code: 1 }))?;

        // Keep the process alive for a bit, so that VSCode doesn't complain about broken pipes.
        tokio::time::sleep(Duration::from_millis(500)).await;

        Err(error)
    }

    /// Waits for the debug adapter to start a session.
    ///
    /// A session is started by the debug adapter sending a `launch` or `attach` request.
    /// This function then handles this request and returns the session data.
    ///
    /// The function exits with no session if a "disconnect" request is received.
    async fn start_session(
        &mut self,
        debug_adapter: &mut DebugAdapter,
        client: &RpcClient,
        timestamp_offset: UtcOffset,
    ) -> Result<Option<SessionData>, DebuggerError> {
        loop {
            // Wait for a request
            let Some(request) = debug_adapter.listen_for_request()? else {
                continue;
            };

            let launch_attach_request = match request.command.as_str() {
                "launch" => TargetSessionType::LaunchRequest,
                "attach" => TargetSessionType::AttachRequest,
                "disconnect" => {
                    debug_adapter.send_response::<DisconnectResponse>(&request, Ok(None))?;
                    return Ok(None);
                }
                _ => {
                    debug_adapter.log_to_console(format!(
                        "Ignoring request with command '{}', we can only handle 'launch' and 'attach' commands.", request.command
                    ));

                    let err = DebuggerError::Other(anyhow!(
                        "Unable to process request with command {} before an attach or launch request is received",
                        request.command
                    ));

                    debug_adapter.send_response::<()>(&request, Err(&err))?;

                    // Continue listening for requests
                    continue;
                }
            };

            self.debug_logger.flush_to_dap(debug_adapter)?;

            // Process either the Launch or Attach request.
            let session = match self
                .handle_launch_attach(
                    &request,
                    launch_attach_request,
                    debug_adapter,
                    client,
                    timestamp_offset,
                )
                .await
            {
                Ok(session_data) => Some(session_data),
                Err(error) => {
                    debug_adapter.send_error_response(&request, &error)?;
                    None
                }
            };

            return Ok(session);
        }
    }

    /// Process launch or attach request
    #[tracing::instrument(skip_all, name = "Handle Launch/Attach Request")]
    async fn handle_launch_attach(
        &mut self,
        launch_attach_request: &Request,
        requested_target_session_type: TargetSessionType,
        debug_adapter: &mut DebugAdapter,
        client: &RpcClient,
        timestamp_offset: UtcOffset,
    ) -> Result<SessionData, DebuggerError> {
        self.config = get_arguments(debug_adapter, launch_attach_request)?;

        self.config
            .validate_configuration_option_compatibility(requested_target_session_type)?;

        debug_adapter
            .set_console_log_level(self.config.console_log_level.unwrap_or(ConsoleLog::Console));

        // Always start each session with a fresh upload area: drop any [`UploadedFiles`] left
        // over from a previous session that did not unwind cleanly (e.g. one that panicked
        // before [`Self::debug_session_impl`] could run its end-of-session cleanup). In the normal
        // case there is nothing to drop here — `debug_session_impl` already cleared it.
        self.uploaded_files = None;

        // In `remote_server_mode`, decode any client-supplied file payloads (program binary, SVD,
        // chip description) and rewrite the corresponding path fields to point at session-scoped
        // temporary files. In local mode this is a no-op. Must run before `validate_config_files`
        // so that the subsequent `is_file()` checks see the materialized paths.
        if self.config.remote_server_mode {
            let uploaded_files = self.uploaded_files.insert(UploadedFiles::new()?);
            self.config.materialize_uploaded_files(uploaded_files)?;
        }

        self.config.validate_config_files()?;

        let mut session_data = SessionData::new_rpc_backed(
            client,
            &mut self.config,
            timestamp_offset,
            self.preattached_session.take(),
        )
        .await?;

        debug_adapter.halt_after_reset = self.config.flashing_config.halt_after_reset;

        let Some(target_core_config) = self.config.core_configs.first() else {
            return Err(DebuggerError::Other(anyhow!(
                "Cannot continue unless one target core configuration is defined."
            )));
        };

        if self.config.flashing_config.flashing_enabled {
            let Some(path_to_elf) = target_core_config.program_binary.clone() else {
                return Err(DebuggerError::Other(anyhow!(
                    "Please specify use the `program-binary` option in `launch.json` to specify an executable"
                )));
            };

            // Store timestamp of flashed binary
            self.binary_timestamp = get_file_timestamp(&path_to_elf);

            Self::flash(
                &self.config,
                &path_to_elf,
                debug_adapter,
                launch_attach_request,
                &mut session_data,
            )
            .await?;
        }

        // First, halt the core so we can finish initialization without
        // bumping into user code. (Depending on `config`, the core is
        // restarted at the end of initialization in `configuration_done`.)
        let core_index = target_core_config.core_index;
        session_data
            .backend
            .halt(core_index, Duration::from_millis(100))
            .await
            .map_err(DebuggerError::from)?;

        // Synchronize the optional SVD configuration before exposing scopes.
        // This is non-fatal: a failed load leaves the server cache cleared.
        let svd_timestamp = target_core_config
            .svd_file
            .as_deref()
            .and_then(get_file_timestamp);
        match session_data
            .backend
            .load_svd(core_index, target_core_config.svd_file.clone())
            .await
        {
            Ok(()) => self.svd_timestamp = svd_timestamp,
            Err(error) => tracing::warn!("Failed to load SVD file: {error:?}"),
        }

        if requested_target_session_type == TargetSessionType::LaunchRequest {
            // This will effectively do a `reset` and `halt` of the core, which is what we want until after the `configuration_done` request.
            debug_adapter
                .restart_async(&mut session_data, core_index, None)
                .await
                .context("Failed to restart core")?;
        }

        session_data.poll_cores(&self.config, debug_adapter).await?;

        debug_adapter.send_response::<()>(launch_attach_request, Ok(None))?;
        self.debug_logger.flush_to_dap(debug_adapter)?;

        Ok(session_data)
    }

    #[tracing::instrument(skip_all)]
    async fn restart(
        &mut self,
        debug_adapter: &mut DebugAdapter,
        session_data: &mut SessionData,
        request: &Request,
    ) -> Result<(), DebuggerError> {
        let Some(target_core_config) = self.config.core_configs.first() else {
            return Err(DebuggerError::Other(anyhow!(
                "Cannot continue unless one target core configuration is defined."
            )));
        };

        if self.config.flashing_config.flashing_enabled {
            let Some(path_to_elf) = target_core_config.program_binary.clone() else {
                return Err(DebuggerError::Other(anyhow!(
                    "Please specify use the `program-binary` option in `launch.json` to specify an executable"
                )));
            };

            let flash_new_binary = match self.binary_timestamp {
                Some(check_current_binary_timestamp) => match get_file_timestamp(&path_to_elf) {
                    Some(new_binary_timestamp) => {
                        if new_binary_timestamp > check_current_binary_timestamp {
                            self.binary_timestamp = Some(new_binary_timestamp);
                            true
                        } else {
                            false
                        }
                    }
                    None => {
                        tracing::warn!(
                            "Could not get timestamp for new binary. Assuming it is new."
                        );
                        true
                    }
                },
                None => {
                    self.binary_timestamp = get_file_timestamp(&path_to_elf);
                    true
                }
            };
            if flash_new_binary {
                // If there is a new binary as part of a restart, there are some key things that
                // need to be 'reset' for things to work properly.
                let core_index = target_core_config.core_index;
                // Reborrow `self.config.core_configs[0]` by cloning the
                // relevant entry so we can call mutating methods on
                // `session_data`.
                let target_core_config = target_core_config.clone();
                // Resolve the upload once so flash and debug-info publication
                // share the same bytes.
                let upload = session_data
                    .backend
                    .session_interface()
                    .resolve_upload(&path_to_elf)
                    .await
                    .map_err(|error| {
                        DebuggerError::Other(anyhow!(
                            "Failed to resolve program binary upload: {error}"
                        ))
                    })?;
                if let Ok(core_data) = session_data.core_data_mut(core_index) {
                    // Reflashing changes the target image. Do not retain
                    // frame ids from the pre-flash server unwind.
                    core_data.invalidate_stack_frame_cache();
                }
                // Flashing can partially mutate the target before returning
                // an error. Invalidate server-derived frame/variable handles
                // first so no failure path can expose pre-flash state.
                session_data
                    .backend
                    .session_interface()
                    .clear_core_debug_state(core_index as u32)
                    .await
                    .map_err(|error| {
                        DebuggerError::Other(anyhow!(
                            "Failed to clear server debug state before reflash: {error}"
                        ))
                    })?;

                Self::flash_resolved(&self.config, &upload, debug_adapter, request, session_data)
                    .await?;

                // Publish the new server-owned DWARF only after flashing
                // succeeds, then recompute source breakpoints through RPC.
                session_data
                    .reload_debug_info_resolved(&target_core_config, &upload)
                    .await?;
                session_data.recompute_breakpoints(core_index).await?;
                session_data.load_rtt_location(&self.config)?;
            }
        }

        // First, halt the core so we can finish restart logic without
        // bumping into user code.
        let core_index = target_core_config.core_index;
        session_data
            .backend
            .halt(core_index, Duration::from_millis(100))
            .await
            .map_err(DebuggerError::from)?;

        // A DAP restart carries no replacement launch configuration, but the
        // configured SVD file may have changed on disk. An SVD parse is slow,
        // thus reload the file only after a change of its timestamp. A failed
        // load clears any stale server cache.
        if let Some(svd_file) = target_core_config.svd_file.clone() {
            let svd_timestamp = get_file_timestamp(&svd_file);
            if svd_timestamp.is_none() || svd_timestamp != self.svd_timestamp {
                match session_data
                    .backend
                    .load_svd(core_index, Some(svd_file))
                    .await
                {
                    // A failed load leaves no SVD data on the server. Forget
                    // the timestamp, so that the next restart tries again.
                    Ok(()) => self.svd_timestamp = svd_timestamp,
                    Err(error) => {
                        self.svd_timestamp = None;
                        tracing::warn!("Failed to reload SVD file during restart: {error:?}");
                    }
                }
            }
        }

        // Reset RTT so that the link can be re-established.
        if let Ok(cd) = session_data.core_data_mut(core_index) {
            cd.rtt_connection = None;
        }

        session_data.clear_rtt_blocks(&self.config).await?;

        // Do not poll the cores here. The reset discards the state of the
        // target, thus a poll would unwind the stack, tell the client that the
        // core halted, and attach to the RTT of the old program for nothing.
        // After completing optional flashing and other config, we can run the debug adapter's restart logic.
        debug_adapter
            .restart_async(session_data, core_index, Some(request))
            .await
            .context("Failed to restart core")?;

        Ok(())
    }

    /// Flash the given binary, and report the progress to the
    /// debug adapter.
    //
    // The actual flashing is delegated to [`RpcBackend::flash_binary_resolved`]
    // so local and remote RPC sessions share this DAP-level progress plumbing.
    async fn flash(
        config: &SessionConfig,
        path_to_elf: &Path,
        debug_adapter: &mut DebugAdapter,
        launch_attach_request: &Request,
        session_data: &mut SessionData,
    ) -> Result<(), DebuggerError> {
        let upload = session_data
            .backend
            .session_interface()
            .resolve_upload(path_to_elf)
            .await
            .map_err(|error| {
                DebuggerError::Other(anyhow!("Failed to resolve program binary upload: {error}"))
            })?;
        Self::flash_resolved(
            config,
            &upload,
            debug_adapter,
            launch_attach_request,
            session_data,
        )
        .await
    }

    /// Flash using a prior [`ResolvedUpload`] so restart validate/flash/publish
    /// share one uploaded object.
    async fn flash_resolved(
        config: &SessionConfig,
        upload: &ResolvedUpload,
        debug_adapter: &mut DebugAdapter,
        launch_attach_request: &Request,
        session_data: &mut SessionData,
    ) -> Result<(), DebuggerError> {
        debug_adapter.log_to_console(format!(
            "FLASHING: Starting write of {} to device memory",
            upload.canonical_path.display()
        ));
        let progress_id = debug_adapter
            .start_progress("Flashing device", Some(launch_attach_request.seq))
            .ok();

        #[derive(Default)]
        struct ProgressBarState {
            total_size: u64,
            size_done: u64,
        }
        type ProgressState = HashMap<Operation, ProgressBarState>;

        // Clear stale RTT control blocks before reflashing so that the old
        // control block header does not leak into the first poll cycle.
        session_data.clear_rtt_blocks(config).await?;

        let mut flash_progress_state = ProgressState::default();
        let describe_op = |operation| match operation {
            Operation::Fill => "Reading Old Pages",
            Operation::Erase => "Erasing Sectors",
            Operation::Program => "Programming Pages",
            Operation::Verify => "Verifying",
            Operation::Ram => "Writing RAM",
        };

        let result = {
            let mut on_event = |event: WireProgressEvent| match event {
                WireProgressEvent::AddProgressBar { operation, total } => {
                    let pbar_state = flash_progress_state.entry(operation).or_default();
                    if let Some(total) = total {
                        pbar_state.total_size += total;
                        pbar_state.size_done = 0;
                    }
                }
                WireProgressEvent::Started(operation) => {
                    if let Some(id) = progress_id {
                        debug_adapter
                            .update_progress(None, Some(describe_op(operation)), id)
                            .ok();
                    }
                }
                WireProgressEvent::Progress { operation, size } => {
                    let pbar_state = flash_progress_state.entry(operation).or_default();
                    pbar_state.size_done += size;
                    let progress =
                        pbar_state.size_done as f64 / pbar_state.total_size.max(1) as f64;

                    if let Some(id) = progress_id {
                        debug_adapter
                            .update_progress(
                                Some(progress.min(1.0)),
                                Some(describe_op(operation)),
                                id,
                            )
                            .ok();
                    }
                }
                WireProgressEvent::Failed(operation) => {
                    if let Some(id) = progress_id {
                        debug_adapter
                            .update_progress(
                                Some(1.0),
                                Some(format!("{} Failed!", describe_op(operation))),
                                id,
                            )
                            .ok();
                    }
                }
                WireProgressEvent::Finished(operation) => {
                    if let Some(id) = progress_id {
                        debug_adapter
                            .update_progress(
                                Some(1.0),
                                Some(format!("{} Complete!", describe_op(operation))),
                                id,
                            )
                            .ok();
                    }
                }
                WireProgressEvent::FlashLayoutReady { .. } => {}
                WireProgressEvent::DiagnosticMessage { .. } => {}
            };

            session_data
                .backend
                .flash_binary_resolved(upload, &config.flashing_config, &mut on_event)
                .await
        };

        if let Some(id) = progress_id {
            let _ = debug_adapter.end_progress(id);
        }

        if result.is_ok() {
            debug_adapter.log_to_console(format!(
                "FLASHING: Completed write of {} to device memory",
                upload.canonical_path.display()
            ));
        }

        result
    }

    #[tracing::instrument(skip_all, name = "Handling initialize request")]
    pub(crate) fn handle_initialize(
        &mut self,
        debug_adapter: &mut DebugAdapter,
    ) -> Result<(), DebuggerError> {
        let initialize_request = loop {
            if let Some(current_request) = debug_adapter.listen_for_request()? {
                if current_request.command == "initialize" {
                    break current_request;
                } else {
                    let error = DebuggerError::Other(anyhow!(
                        "Received request with command'{}', expected to receive the initialize command",
                        current_request.command,
                    ));
                    debug_adapter.send_response::<()>(&current_request, Err(&error))?;
                    return Err(error);
                }
            }
        };

        let initialize_arguments =
            get_arguments::<InitializeRequestArguments>(debug_adapter, &initialize_request)?;

        // Enable quirks specific to particular DAP clients...
        if let Some(client_id) = initialize_arguments.client_id
            && client_id == "vscode"
        {
            tracing::info!(
                "DAP client reports its 'ClientID' is 'vscode', enabling vscode_quirks."
            );
            debug_adapter.vscode_quirks = true;
        }

        if !(initialize_arguments.columns_start_at_1.unwrap_or(true)
            && initialize_arguments.lines_start_at_1.unwrap_or(true))
        {
            let error = DebuggerError::Other(anyhow!(
                "Unsupported Capability: Client requested column and row numbers start at 0."
            ));
            debug_adapter.send_response::<()>(&initialize_request, Err(&error))?;
            return Err(error);
        }

        if let Some(progress_support) = initialize_arguments.supports_progress_reporting {
            debug_adapter.supports_progress_reporting = progress_support;
        }

        if let Some(ansi_styling) = initialize_arguments.supports_ansi_styling {
            debug_adapter.supports_ansi_styling = ansi_styling;
        }

        if let Some(lines_start_at_1) = initialize_arguments.lines_start_at_1 {
            debug_adapter.lines_start_at_1 = lines_start_at_1;
        }

        if let Some(columns_start_at_1) = initialize_arguments.columns_start_at_1 {
            debug_adapter.columns_start_at_1 = columns_start_at_1;
        }

        // Reply to Initialize with `Capabilities`.
        let capabilities = dap_capabilities();
        debug_adapter.send_response(&initialize_request, Ok(Some(capabilities)))?;

        self.debug_logger.flush_to_dap(debug_adapter)?;

        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod test {

    use crate::cmd::dap_server::{
        DebuggerError,
        debug_adapter::{
            dap::{
                adapter::DebugAdapter,
                dap_types::{
                    Capabilities, ContinuedEventBody, DisassembleArguments,
                    DisassembleResponseBody, DisassembledInstruction, DisconnectArguments,
                    ErrorResponseBody, InitializeRequestArguments, Message, OutputEventBody,
                    Request, Response, Source, Thread, ThreadsResponseBody, VariablesArguments,
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
        probe::{DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, ProbeFactory},
    };
    use probe_rs_rpc_client::RpcClient;
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

        fn list_probes(&self) -> Vec<probe_rs::probe::list::ProbeListItem> {
            todo!()
        }
    }

    /// Helper function to get the expected capabilities for the debugger
    ///
    /// `Capabilities::default()` is not const, so this can't just be a constant.
    fn expected_capabilities() -> Capabilities {
        super::dap_capabilities()
    }

    #[test]
    fn initialize_capabilities_match_dispatch_and_behavior() {
        let capabilities = super::dap_capabilities();

        // Request capabilities with explicit handle_request arms.
        assert_eq!(capabilities.supports_configuration_done_request, Some(true));
        assert_eq!(capabilities.supports_restart_request, Some(true));
        assert_eq!(capabilities.supports_read_memory_request, Some(true));
        assert_eq!(capabilities.supports_write_memory_request, Some(true));
        assert_eq!(capabilities.supports_set_variable, Some(true));
        assert_eq!(capabilities.supports_disassemble_request, Some(true));
        assert_eq!(capabilities.supports_instruction_breakpoints, Some(true));
        assert_eq!(capabilities.supports_completions_request, Some(true));

        // Behavior capabilities implemented by existing request handlers.
        assert_eq!(capabilities.supports_evaluate_for_hovers, Some(true));
        assert_eq!(capabilities.supports_stepping_granularity, Some(true));
        assert_eq!(capabilities.support_suspend_debuggee, Some(true));
        assert_eq!(capabilities.support_terminate_debuggee, Some(true));
        assert_eq!(capabilities.supports_ansi_styling, Some(true));
        assert_eq!(
            capabilities.supports_delayed_stack_trace_loading,
            Some(false)
        );

        // These pre-existing requests still take the fallback path and must
        // remain unadvertised.
        assert_ne!(capabilities.supports_terminate_request, Some(true));
        assert_ne!(capabilities.supports_modules_request, Some(true));
        assert_ne!(capabilities.supports_loaded_sources_request, Some(true));
        assert_ne!(capabilities.supports_exception_info_request, Some(true));
        assert_ne!(capabilities.supports_exception_options, Some(true));
        assert_ne!(capabilities.supports_exception_filter_options, Some(true));
        assert!(capabilities.exception_breakpoint_filters.is_none());
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
            supports_ansi_styling: None,
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

        fn and_successful_response(self) -> ResponseBuilder<'r> {
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

        fn expect_response(&mut self, response: Response) -> ResponseBuilder<'_> {
            assert!(
                response.success,
                "success field must be true for successful response"
            );
            self.expected_responses.push(response);
            ResponseBuilder { adapter: self }
        }

        fn expect_error_response(&mut self, response: Response) -> ResponseBuilder<'_> {
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

        fn dyn_send_event(
            &mut self,
            event_type: &str,
            event_body: Option<serde_json::Value>,
        ) -> anyhow::Result<()> {
            tracing::debug!("Sending event: {}", event_type);
            if self.event_index >= self.expected_events.len() {
                panic!(
                    "No more events expected, but got event_type={event_type:?}, event_body={event_body:?}"
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

        fn send_raw_response(&mut self, response: Response) -> anyhow::Result<()> {
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

        fn has_pending_request(&self, request_seq: i64) -> bool {
            self.pending_requests.contains_key(&request_seq)
        }

        fn get_next_seq(&mut self) -> i64 {
            self.sequence_number += 1;
            self.sequence_number
        }
    }

    fn initialized_protocol_adapter() -> MockProtocolAdapter {
        let mut protocol_adapter = MockProtocolAdapter::new();

        protocol_adapter
            .add_request("initialize")
            .with_arguments(default_initialize_args())
            .and_successful_response()
            .with_body(expected_capabilities());

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
            .and_successful_response();

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
            .and_successful_response();
    }

    fn fake_probe() -> (DebugProbeInfo, FakeProbe) {
        let probe_info = DebugProbeInfo::new(
            "Mock probe",
            0x12,
            0x23,
            Some("mock_serial".to_owned()),
            &MockProbeFactory,
            None,
            false,
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

    async fn execute_test(
        protocol_adapter: MockProtocolAdapter,
        with_probe: bool,
    ) -> Result<(), DebuggerError> {
        use crate::rpc::functions::RpcApp;
        use std::sync::Arc;

        let debug_adapter = DebugAdapter::new(protocol_adapter);

        let lister = TestLister::new();
        if with_probe {
            lister.probes.lock().unwrap().push(fake_probe());
        }
        let lister = Arc::new(lister) as Arc<dyn probe_rs::integration::ProbeLister + Send + Sync>;

        // Spawn an in-process RPC server backed by the test lister (so the
        // `FakeProbe` is visible to `probe/attach`), and drive the DAP
        // session through `RpcBackend` — the same path production uses.
        let probe_broker = Arc::new(crate::rpc::probe_broker::ProbeBroker::new());
        let (local_server, tx, rx) = RpcApp::create_server_with_lister(16, lister, probe_broker);
        let handle = tokio::spawn(async move { local_server.run().await });

        let client = RpcClient::new_local_from_wire(tx, rx);
        let mut debugger = Debugger::new(UtcOffset::UTC, None)?;
        let result = debugger.debug_session_rpc(&client, debug_adapter).await;

        // Shut the server down (drop the client first so the server exits).
        drop(client);
        _ = handle.await;
        result
    }

    #[tokio::test]
    async fn test_initialize_request() {
        let protocol_adapter = initialized_protocol_adapter();

        // TODO: Check proper return value
        execute_test(protocol_adapter, false).await.unwrap_err();
    }

    #[tokio::test]
    async fn test_launch_no_probes() {
        let mut protocol_adapter = initialized_protocol_adapter();

        let expected_error = "No connected probes were found.";
        protocol_adapter.expect_output_event(&format!("{expected_error}\n"));

        // RPC attach requires a chip name (to look up the target locally and
        // drive `probe/attach`), so give it a valid session config. With no
        // `FakeProbe` registered in the test lister, `select_probe` then
        // reports "No connected probes were found." (before the program
        // binary is ever touched).
        protocol_adapter
            .add_request("launch")
            .with_arguments(valid_session_config())
            .and_error_response()
            .with_body(error_response_body(expected_error));

        execute_test(protocol_adapter, false).await.unwrap();
    }

    #[tokio::test]
    async fn test_launch_and_terminate() {
        let mut protocol_adapter = launched_protocol_adapter();

        disconnect_protocol_adapter(&mut protocol_adapter);

        execute_test(protocol_adapter, true).await.unwrap();
    }

    #[tokio::test]
    async fn wrong_request_after_init() {
        let mut protocol_adapter = initialized_protocol_adapter();

        let expected_error = "Unable to process request with command threads before an attach or launch request is received";
        protocol_adapter.expect_output_event("Ignoring request with command 'threads', we can only handle 'launch' and 'attach' commands.\n");

        protocol_adapter
            .add_request("threads")
            .and_error_response()
            .with_body(error_response_body(expected_error));

        protocol_adapter.expect_output_event("Unable to process request with command threads before an attach or launch request is received\n");

        disconnect_protocol_adapter(&mut protocol_adapter);

        execute_test(protocol_adapter, true).await.unwrap();
    }

    #[tokio::test]
    async fn attach_request() {
        let mut protocol_adapter = initialized_protocol_adapter();

        let attach_args = valid_session_config();
        protocol_adapter
            .add_request("attach")
            .with_arguments(attach_args)
            .and_successful_response();

        protocol_adapter.expect_event("initialized", None::<u32>);

        disconnect_protocol_adapter(&mut protocol_adapter);

        execute_test(protocol_adapter, true).await.unwrap();
    }

    #[tokio::test]
    async fn attach_with_flashing() {
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

        execute_test(protocol_adapter, true).await.unwrap();
    }

    #[tokio::test]
    async fn launch_and_threads() {
        let mut protocol_adapter = launched_protocol_adapter();

        protocol_adapter
            .add_request("configurationDone")
            .and_successful_response();

        protocol_adapter.expect_event(
            "continued",
            Some(ContinuedEventBody {
                all_threads_continued: Some(true),
                thread_id: 0,
            }),
        );
        protocol_adapter.expect_event(
            "output",
            Some(OutputEventBody {
                output: String::from("Core is running\n"),
                category: Some("console".to_owned()),
                variables_reference: None,
                source: None,
                line: None,
                column: None,
                data: None,
                group: Some("probe-rs-debug".to_owned()),
                location_reference: None,
            }),
        );

        protocol_adapter
            .add_request("threads")
            .and_successful_response()
            .with_body(ThreadsResponseBody {
                threads: vec![Thread {
                    id: 0,
                    name: format!("0-{TEST_CHIP_NAME}"),
                }],
            });

        disconnect_protocol_adapter(&mut protocol_adapter);

        execute_test(protocol_adapter, true).await.unwrap();
    }

    /// A request that fails must receive an error response, and must not end
    /// the session.
    #[tokio::test]
    async fn failed_request_does_not_end_the_session() {
        let mut protocol_adapter = launched_protocol_adapter();

        protocol_adapter
            .add_request("configurationDone")
            .and_successful_response();

        protocol_adapter.expect_event(
            "continued",
            Some(ContinuedEventBody {
                all_threads_continued: Some(true),
                thread_id: 0,
            }),
        );
        protocol_adapter.expect_output_event("Core is running\n");

        let unknown_variables_reference = 0xDEAD_BEEF_i64;
        let expected_error =
            format!("No variable information found for {unknown_variables_reference}!");
        protocol_adapter
            .add_request("variables")
            .with_arguments(VariablesArguments {
                variables_reference: unknown_variables_reference,
                count: None,
                filter: None,
                format: None,
                start: None,
            })
            .and_error_response()
            .with_body(error_response_body(&expected_error));
        protocol_adapter.expect_output_event(&format!("{expected_error}\n"));

        // The session must still answer the requests that follow.
        protocol_adapter
            .add_request("threads")
            .and_successful_response()
            .with_body(ThreadsResponseBody {
                threads: vec![Thread {
                    id: 0,
                    name: format!("0-{TEST_CHIP_NAME}"),
                }],
            });

        disconnect_protocol_adapter(&mut protocol_adapter);

        execute_test(protocol_adapter, true).await.unwrap();
    }

    #[test_case(0; "instructions before and not including the ref address, multiple locations")]
    #[test_case(1; "instructions including the ref address, location cloned from earlier line")]
    #[test_case(2; "instructions after and not including the ref address")]
    #[test_case(3; "negative byte offset of exactly one instruction (aligned)")]
    #[test_case(4; "positive byte offset that lands in the middle of an instruction (unaligned)")]
    #[tokio::test]
    async fn disassemble(test_case: usize) {
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

            // The DAP server emits source paths from DWARF debug information verbatim. The path
            // is the build-time path recorded by `rustc` (a synthetic `/rustc/<hash>/...` for
            // precompiled rustlib sources), and `presentation_hint` is left unset. Mapping such
            // synthetic paths to a usable on-disk location is the VSCode extension's job, not the
            // server's.
            type TestLocation = (i64, i64, &'static str, &'static str);
            const TEST_LOCATIONS: [TestLocation; 3] = [
                // line, column, name, path
                (115, 5, "ub_checks.rs", "/rustc/7f2fc33da6633f5a764ddc263c769b6b2873d167/library/core/src/ub_checks.rs"),
                (0, 5, "ub_checks.rs", "/rustc/7f2fc33da6633f5a764ddc263c769b6b2873d167/library/core/src/ub_checks.rs"),
                (1244, 5, "mod.rs", "/rustc/7f2fc33da6633f5a764ddc263c769b6b2873d167/library/core/src/num/mod.rs"),
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
            .and_successful_response();

        protocol_adapter.expect_event(
            "continued",
            Some(ContinuedEventBody {
                all_threads_continued: Some(true),
                thread_id: 0,
            }),
        );
        protocol_adapter.expect_event(
            "output",
            Some(OutputEventBody {
                output: String::from("Core is running\n"),
                category: Some("console".to_owned()),
                variables_reference: None,
                source: None,
                line: None,
                column: None,
                data: None,
                group: Some("probe-rs-debug".to_owned()),
                location_reference: None,
            }),
        );

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
            presentation_hint: None,
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
            .and_successful_response()
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
                        if let Some(&(line, column, name, path)) = test_locs.get(address) {
                            instruction.line = if *line == 0 { None } else { Some(*line) };
                            instruction.column = Some(*column);
                            instruction.location = Some(Source {
                                name: Some(name.to_string()),
                                path: Some(path.to_string()),
                                ..default_source_fields.clone()
                            })
                        }
                        instruction
                    })
                    .collect(),
            });

        disconnect_protocol_adapter(&mut protocol_adapter);

        execute_test(protocol_adapter, true).await.unwrap();
    }
}
