use std::any::Any;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::{ops::Range, path::Path};

use super::session_data::{self, ActiveBreakpoint, BreakpointType, SourceLocationScope};
use crate::cmd::dap_server::debug_adapter::dap::dap_types::MessageSeverity;
use crate::cmd::dap_server::debug_adapter::dap::repl_commands::ReplCommand;
use crate::util::rtt::client::RttClient;
use crate::util::rtt::{self, DataFormat, DefmtProcessor, DefmtState};
use crate::{
    cmd::dap_server::{
        DebuggerError,
        debug_adapter::{
            dap::{
                adapter::DebugAdapter,
                core_status::DapStatus,
                dap_types::{ContinuedEventBody, Source, StoppedEventBody},
            },
            protocol::ProtocolAdapter,
        },
        peripherals::svd_variables::SvdCache,
        server::debug_rtt,
    },
    util::rtt::RttDecoder,
};
use anyhow::{Result, anyhow};
use probe_rs::semihosting::SemihostingCommand;
use probe_rs::{Architecture, BreakpointCause, BreakpointError, Error, MemoryInterface as _};
use probe_rs::{Core, CoreStatus, HaltReason, rtt::ScanRegion};
use probe_rs_debug::VerifiedBreakpoint;
use probe_rs_debug::{
    ColumnType, ObjectRef, VariableCache, debug_info::DebugInfo, stack_frame::StackFrameInfo,
};
use time::UtcOffset;
use typed_path::TypedPath;

/// [CoreData] is used to cache data needed by the debugger, on a per-core basis.
pub struct CoreData {
    pub core_index: usize,
    /// Track the last_known_status of the core.
    /// The debug client needs to be notified when the core changes state, and this can happen in one of two ways:
    /// 1. By polling the core status periodically (in [`crate::cmd::dap_server::server::debugger::Debugger::process_next_request()`]).
    ///    For instance, when the client sets the core running, and the core halts because of a breakpoint, we need to notify the client.
    /// 2. Some requests, like [`DebugAdapter::next()`], has an implicit action of setting the core running, before it waits for it to halt at the next statement.
    ///    To ensure the [`CoreHandle::poll_core()`] behaves correctly, it will set the `last_known_status` to [`CoreStatus::Running`],
    ///    and execute the request normally, with the expectation that the core will be halted, and that 1. above will detect this new status.
    ///    These 'implicit' updates of `last_known_status` will not(and should not) result in a notification to the client.
    pub last_known_status: CoreStatus,
    pub target_name: String,
    pub debug_info: Option<DebugInfo>,
    pub static_variables: Option<VariableCache>,
    pub core_peripherals: Option<SvdCache>,
    pub stack_frames: Vec<probe_rs_debug::stack_frame::StackFrame>,
    pub breakpoints: Vec<session_data::ActiveBreakpoint>,
    pub rtt_scan_ranges: ScanRegion,
    pub rtt_connection: Option<debug_rtt::RttConnection>,
    pub rtt_client: Option<RttClient>,
    pub clear_rtt_header: bool,
    pub rtt_header_cleared: bool,
    pub next_semihosting_handle: u32,
    pub semihosting_handles: HashMap<u32, SemihostingFile>,
    pub repl_commands: Vec<ReplCommand>,
    pub test_data: Box<dyn Any>,
}

/// File descriptor for files opened by the target.
pub struct SemihostingFile {
    handle: NonZeroU32,
    path: String,
    mode: &'static str,
}

/// [CoreHandle] provides handles to various data structures required to debug a single instance of a core. The actual state is stored in [session_data::SessionData].
///
/// Usage: To get access to this structure please use the [session_data::SessionData::attach_core] method. Please keep access/locks to this to a minimum duration.
pub struct CoreHandle<'p> {
    pub(crate) core_id: usize,
    pub(crate) core: Core<'p>,
    pub(crate) core_data: &'p mut CoreData,
}

impl CoreHandle<'_> {
    pub(crate) fn id(&self) -> usize {
        self.core_id
    }

    /// Some MS DAP requests (e.g. `step`) implicitly expect the core to resume processing and then to optionally halt again, before the request completes.
    ///
    /// This method is used to set the `last_known_status` to [`CoreStatus::Unknown`] (because we cannot verify that it will indeed resume running until we have polled it again),
    ///   as well as [`DebugAdapter::all_cores_halted`] = `false`, without notifying the client of any status changes.
    pub(crate) fn reset_core_status<P: ProtocolAdapter + ?Sized>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
    ) {
        self.core_data.last_known_status = CoreStatus::Unknown;
        debug_adapter.all_cores_halted = false;
    }

    /// - Whenever we check the status, we compare it against `last_known_status` and send the appropriate event to the client.
    /// - If we cannot determine the core status, then there is no sense in continuing the debug session, so please propagate the error.
    /// - If the core status has changed, then we update `last_known_status` to the new value, and return `true` as part of the Result<>.
    pub(crate) fn poll_core<P: ProtocolAdapter>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
    ) -> Result<CoreStatus, DebuggerError> {
        if !debug_adapter.configuration_is_done() {
            tracing::trace!(
                "Ignored last_known_status: {:?} during `configuration_done=false`, and reset it to {:?}.",
                self.core_data.last_known_status,
                CoreStatus::Unknown
            );
            return Ok(CoreStatus::Unknown);
        }

        let status = match self.core.status() {
            Ok(status) => {
                if status == self.core_data.last_known_status {
                    return Ok(status);
                }

                status
            }
            Err(error) => {
                self.core_data.last_known_status = CoreStatus::Unknown;
                return Err(error.into());
            }
        };

        // Update this unconditionally, because halted() can have more than one variant.
        self.core_data.last_known_status = status;

        match status {
            CoreStatus::Running | CoreStatus::Sleeping => {
                let event_body = Some(ContinuedEventBody {
                    all_threads_continued: Some(true), // TODO: Implement multi-core awareness here
                    thread_id: self.id() as i64,
                });
                debug_adapter.send_event("continued", event_body)?;
                tracing::trace!("Notified DAP client that the core continued: {:?}", status);
            }

            CoreStatus::Halted(HaltReason::Step) => {
                // HaltReason::Step is a special case, where we have to send a custom event to the client that the core halted.
                // In this case, we don't re-send the "stopped" event, but further down, we will
                // update the `last_known_status` to the actual HaltReason returned by the core.
            }

            CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(_))) => {
                // We handle semihosting calls without sending a "stopped" event. The core will
                // be resumed after the semihosting command is handled, unless the command
                // is not handled or indicates that the core should halt.
            }

            CoreStatus::Halted(_) => self.notify_halted(debug_adapter, status)?,
            CoreStatus::LockedUp => {
                // TODO: We can't really continue here, but the debugger should remain working
                //
                // Maybe step should be prevented?

                debug_adapter.show_message(
                    MessageSeverity::Warning,
                    format!("Core {} is in locked up state", self.core_id),
                );

                self.notify_halted(debug_adapter, status)?
            }
            CoreStatus::Unknown => {
                let error =
                    DebuggerError::Other(anyhow!("Unknown Device status received from Probe-rs"));
                debug_adapter.show_error_message(&error)?;

                return Err(error);
            }
        }

        Ok(status)
    }

    /// Search available [`probe_rs::debug::StackFrame`]'s for the given `id`
    pub(crate) fn get_stackframe(
        &self,
        id: ObjectRef,
    ) -> Option<&probe_rs_debug::stack_frame::StackFrame> {
        self.core_data
            .stack_frames
            .iter()
            .find(|stack_frame| stack_frame.id == id)
    }

    /// Confirm RTT initialization on the target, and use the RTT channel configurations to initialize the output windows on the DAP Client.
    pub fn attach_to_rtt<P: ProtocolAdapter>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
        program_binary: Option<&Path>,
        rtt_config: &rtt::RttConfig,
        timestamp_offset: UtcOffset,
    ) -> Result<()> {
        // We're done already, don't process everything again for no good reason.
        if self.core_data.rtt_connection.is_some() {
            return Ok(());
        }

        let core_id = self.id();
        let client = if let Some(client) = self.core_data.rtt_client.as_mut() {
            client
        } else {
            self.core_data.rtt_header_cleared = false;
            self.core_data.rtt_client.insert(RttClient::new(
                rtt_config.clone(),
                self.core_data.rtt_scan_ranges.clone(),
                self.core.target(),
            ))
        };

        if client.core_id() != core_id {
            return Ok(());
        }

        if self.core_data.clear_rtt_header && !self.core_data.rtt_header_cleared {
            client.clear_control_block(&mut self.core)?;
            self.core_data.rtt_header_cleared = true;
            // Trigger a reattach
            return Ok(());
        }

        let Ok(true) = client.try_attach(&mut self.core) else {
            return Ok(());
        };

        // Now that we're attached, we can transform our state.
        let Some(client) = self.core_data.rtt_client.take() else {
            return Ok(());
        };

        let mut debugger_rtt_channels = vec![];

        let mut defmt_data = None;
        let use_auto_formats = rtt_config.channels.is_empty();

        for up_channel in client.up_channels() {
            let number = up_channel.up_channel.number();
            let channel_name = up_channel.channel_name();

            let mut channel_config = rtt_config.channel_config(number as u32).clone();

            if use_auto_formats {
                channel_config.data_format = if channel_name == "defmt" {
                    DataFormat::Defmt
                } else {
                    DataFormat::String
                };
            }

            // Where `channel_config` is unspecified, apply default from `default_channel_config`.
            let show_timestamps = channel_config.show_timestamps;
            let show_location = channel_config.show_location;
            let log_format = channel_config.log_format.clone();

            let channel_data_format = match channel_config.data_format {
                DataFormat::String => RttDecoder::String {
                    timestamp_offset: Some(timestamp_offset),
                    last_line_done: false,
                    show_timestamps,
                },
                DataFormat::BinaryLE => RttDecoder::BinaryLE,
                DataFormat::Defmt => {
                    let defmt_state = if let Some(data) = defmt_data.as_ref() {
                        data
                    } else if let Some(program_binary) = program_binary {
                        // Create the RTT client using the RTT control block address from the ELF file.
                        let elf = std::fs::read(program_binary).map_err(|error| {
                            anyhow!("Error attempting to attach to RTT: {error}")
                        })?;
                        defmt_data.insert(DefmtState::try_from_bytes(&elf)?)
                    } else {
                        defmt_data.insert(None)
                    };

                    match defmt_state {
                        Some(defmt_state) => RttDecoder::Defmt {
                            processor: DefmtProcessor::new(
                                defmt_state.clone(),
                                show_timestamps,
                                show_location,
                                log_format.as_deref(),
                            ),
                        },
                        None => RttDecoder::BinaryLE,
                    }
                }
            };

            let data_format = DataFormat::from(&channel_data_format);

            debugger_rtt_channels.push(debug_rtt::DebuggerRttChannel {
                channel_number: up_channel.number(),
                // This value will eventually be set to true by a VSCode client request "rttWindowOpened"
                has_client_window: false,
                channel_data_format,
            });

            debug_adapter.rtt_window(up_channel.number(), channel_name, data_format);
        }

        self.core_data.rtt_connection = Some(debug_rtt::RttConnection {
            client,
            debugger_rtt_channels,
        });

        Ok(())
    }

    /// Check if a breakpoint address is already cached in [`CoreData::breakpoints`].
    /// Use this to avoid duplicate breakpoint entries, and also to help with clearing existing breakpoints on request.
    fn find_breakpoint_in_cache(&self, address: u64) -> Option<(usize, &ActiveBreakpoint)> {
        self.core_data
            .breakpoints
            .iter()
            .enumerate()
            .find(|(_, breakpoint)| breakpoint.address == address)
    }

    /// Set a single breakpoint in target configuration as well as [`super::core_data::CoreHandle`]
    pub(crate) fn set_breakpoint(
        &mut self,
        address: u64,
        breakpoint_type: session_data::BreakpointType,
    ) -> Result<(), DebuggerError> {
        // NOTE: After receiving a DAP [`crate::debug_adapter::dap::dap_types::BreakpointEvent`], VSCode will mistakenly
        // identify a `InstructionBreakpoint` as a `SourceBreakpoint`. This results in breakpoints not being cleared correctly from [`CoreHandle::clear_breakpoints()`].
        // To work around this, we have to clear the breakpoints manually before we set them again.
        if let Some((_, breakpoint)) = self.find_breakpoint_in_cache(address) {
            self.clear_breakpoint(breakpoint.address)?;
        }

        self.core
            .set_hw_breakpoint(address)
            .map_err(DebuggerError::ProbeRs)?;
        // Wait until the set of the hw breakpoint succeeded, before we cache it here ...
        self.core_data
            .breakpoints
            .push(session_data::ActiveBreakpoint {
                breakpoint_type,
                address,
            });
        Ok(())
    }

    /// Clear a single breakpoint from target configuration.
    ///
    /// Returns whether the breakpoint was successfully cleared.
    pub(crate) fn clear_breakpoint(&mut self, address: u64) -> Result<bool> {
        match self.core.clear_hw_breakpoint(address) {
            Ok(_) => {}
            Err(probe_rs::Error::BreakpointOperation(BreakpointError::NotFound(_addr))) => {}
            Err(e) => return Err(DebuggerError::ProbeRs(e).into()),
        }
        if let Some((breakpoint_position, _)) = self.find_breakpoint_in_cache(address) {
            self.core_data.breakpoints.remove(breakpoint_position);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Clear all breakpoints of a specified [`super::session_data::BreakpointType`].
    /// Affects target configuration as well as [`CoreData::breakpoints`].
    /// If `breakpoint_type` is of type [`super::session_data::BreakpointType::SourceBreakpoint`], then all breakpoints for the contained [`Source`] will be cleared.
    pub(crate) fn clear_breakpoints(
        &mut self,
        breakpoint_type: session_data::BreakpointType,
    ) -> Result<()> {
        let target_breakpoints = self
            .core_data
            .breakpoints
            .iter()
            .filter(|target_breakpoint| {
                target_breakpoint.breakpoint_type == breakpoint_type
                 || matches!(
                        &target_breakpoint.breakpoint_type,
                        BreakpointType::SourceBreakpoint{source: breakpoint_source, location: _}
                            if matches!(&breakpoint_type, BreakpointType::SourceBreakpoint{source: clear_breakpoint_source, ..}
                                if clear_breakpoint_source == breakpoint_source)
                    )
            })
            .map(|breakpoint| breakpoint.address)
            .collect::<Vec<u64>>();
        for breakpoint in target_breakpoints {
            self.clear_breakpoint(breakpoint)?;
        }
        Ok(())
    }

    /// Set a breakpoint at the requested address. If the requested source location is not specific, or
    /// if the requested address is not a valid breakpoint location,
    /// the debugger will attempt to find the closest location to the requested location, and set a breakpoint there.
    /// The Result<> contains the "verified" `address` and `SourceLocation` where the breakpoint that was set.
    pub(crate) fn verify_and_set_breakpoint(
        &mut self,
        source_path: TypedPath,
        requested_breakpoint_line: u64,
        requested_breakpoint_column: Option<u64>,
        requested_source: &Source,
    ) -> Result<VerifiedBreakpoint, DebuggerError> {
        let Some(ref debug_info) = self.core_data.debug_info else {
            return Err(DebuggerError::Other(anyhow!(
                "Cannot set source breakpoint without debug information."
            )));
        };

        let VerifiedBreakpoint {
                 address,
                 source_location,
             } = debug_info
            .get_breakpoint_location(
                source_path,
                requested_breakpoint_line,
                requested_breakpoint_column,
            )
            .map_err(|debug_error|
                DebuggerError::Other(anyhow!("Cannot set breakpoint here. Try reducing compile time-, and link time-, optimization in your build configuration, or choose a different source location: {debug_error}")))?;
        self.set_breakpoint(
            address,
            BreakpointType::SourceBreakpoint {
                source: Box::new(requested_source.clone()),
                location: SourceLocationScope::Specific(source_location.clone()),
            },
        )?;
        Ok(VerifiedBreakpoint {
            address,
            source_location,
        })
    }

    /// In the case where a new binary is flashed as part of a restart, we need to recompute the breakpoint address,
    /// for a specified source location, of any [`super::session_data::BreakpointType::SourceBreakpoint`].
    /// This is because the address of the breakpoint may have changed based on changes in the source file that created the new binary.
    pub(crate) fn recompute_breakpoints(&mut self) -> Result<(), DebuggerError> {
        if self.core_data.debug_info.is_none() {
            return Ok(());
        }
        let target_breakpoints = self.core_data.breakpoints.clone();
        for breakpoint in target_breakpoints
            .iter()
            .filter(|&breakpoint| {
                matches!(
                    breakpoint.breakpoint_type,
                    BreakpointType::SourceBreakpoint { .. }
                )
            })
            .cloned()
        {
            self.clear_breakpoint(breakpoint.address)?;
            if let BreakpointType::SourceBreakpoint {
                source,
                location: SourceLocationScope::Specific(source_location),
            } = breakpoint.breakpoint_type
            {
                let breakpoint_err = self.verify_and_set_breakpoint(
                    source_location.path.to_path(),
                    source_location.line.unwrap_or(0),
                    source_location.column.map(|col| match col {
                        ColumnType::LeftEdge => 0_u64,
                        ColumnType::Column(c) => c,
                    }),
                    &source,
                );

                if let Err(breakpoint_error) = breakpoint_err {
                    return Err(DebuggerError::Other(anyhow!(
                        "Failed to recompute breakpoint at {source_location:?} in {source:?}. Error: {breakpoint_error:?}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Traverse all the variables in the available stack frames, and return the memory ranges
    /// required to resolve the values of these variables. This is used to provide the minimal
    /// memory ranges required to create a [`CoreDump`](probe_rs::CoreDump) for the current scope.
    pub(crate) fn get_memory_ranges(&mut self) -> Vec<Range<u64>> {
        let recursion_limit = 10;

        let mut all_discrete_memory_ranges = Vec::new();

        if let Some(static_variables) = &mut self.core_data.static_variables
            && let Some(debug_info) = self.core_data.debug_info.as_ref()
        {
            static_variables.recurse_deferred_variables(
                debug_info,
                &mut self.core,
                recursion_limit,
                StackFrameInfo {
                    registers: &self.core_data.stack_frames[0].registers,
                    frame_base: self.core_data.stack_frames[0].frame_base,
                    canonical_frame_address: self.core_data.stack_frames[0].canonical_frame_address,
                },
            );
            all_discrete_memory_ranges.append(&mut static_variables.get_discrete_memory_ranges());
        }

        // Expand and validate the static and local variables for each stack frame.
        for frame in self.core_data.stack_frames.iter_mut() {
            let mut variable_caches = Vec::new();
            if let Some(local_variables) = &mut frame.local_variables {
                variable_caches.push(local_variables);
            }
            for variable_cache in variable_caches {
                if let Some(debug_info) = self.core_data.debug_info.as_ref() {
                    // Cache the deferred top level children of the of the cache.
                    variable_cache.recurse_deferred_variables(
                        debug_info,
                        &mut self.core,
                        10,
                        StackFrameInfo {
                            registers: &frame.registers,
                            frame_base: frame.frame_base,
                            canonical_frame_address: frame.canonical_frame_address,
                        },
                    );
                    all_discrete_memory_ranges
                        .append(&mut variable_cache.get_discrete_memory_ranges());
                }
            }
            // Also capture memory addresses for essential registers.
            for register in frame.registers.0.iter() {
                if let Ok(Some(memory_range)) = register.memory_range() {
                    all_discrete_memory_ranges.push(memory_range);
                }
            }
        }
        // Consolidating all memory ranges that are withing 0x400 bytes of each other.
        consolidate_memory_ranges(all_discrete_memory_ranges, 0x400)
    }

    pub fn handle_semihosting<P: ProtocolAdapter>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
        command: SemihostingCommand,
    ) -> Result<CoreStatus, DebuggerError> {
        match command {
            SemihostingCommand::Open(request) => {
                tracing::debug!("Semihosting request: open {request:?}");
                let path = request.path(&mut self.core)?;
                let mode = request.mode();

                let is_write = mode.starts_with('w') || mode.starts_with('a');
                let is_append = mode.starts_with('a');
                let is_stdio = path == ":tt";

                let path = if is_stdio {
                    if is_append { "stderr" } else { "stdout" }
                } else {
                    &path
                };

                let is_binary = mode.ends_with('b');
                let format = if is_binary {
                    DataFormat::BinaryLE
                } else {
                    DataFormat::String
                };

                // We don't handle writing to the target.
                if is_write {
                    // Reuse handle based on path.
                    if let Some(file) = self
                        .core_data
                        .semihosting_handles
                        .values()
                        .find(|f| f.path == path)
                    {
                        request.respond_with_handle(&mut self.core, file.handle)?;
                    } else {
                        // If the handle is not found, we create a new one.
                        // The handle is a u32, starting from 1024.
                        // We will use the path as the key to store the handle.
                        // This way, we can reuse the same handle for the same path.
                        let handle = self.core_data.next_semihosting_handle;
                        #[expect(
                            clippy::unwrap_used,
                            reason = "Infallible because we start from 1024"
                        )]
                        let nz_handle = NonZeroU32::new(handle).unwrap();
                        self.core_data.semihosting_handles.insert(
                            handle,
                            SemihostingFile {
                                handle: nz_handle,
                                path: path.to_string(),
                                mode,
                            },
                        );
                        self.core_data.next_semihosting_handle += 1;

                        if debug_adapter.rtt_window(handle, path.to_string(), format) {
                            request.respond_with_handle(&mut self.core, nz_handle)?;
                        }
                    }
                }
            }
            SemihostingCommand::Close(request) => {
                tracing::debug!("Semihosting request: close {request:?}");
                request.success(&mut self.core)?;
            }
            SemihostingCommand::WriteConsole(request) => {
                tracing::debug!("Semihosting request: write console {request:?}");
                let string = request.read(&mut self.core)?;
                debug_adapter.log_to_console(string);
            }
            SemihostingCommand::Write(request) => {
                tracing::debug!("Semihosting request: write {request:?}");
                let handle = request.file_handle();
                let bytes = request.read(&mut self.core)?;

                if let Some(file) = self.core_data.semihosting_handles.get(&handle) {
                    let data = if file.mode.ends_with('b') {
                        let mut string = String::new();
                        for byte in bytes {
                            if !string.is_empty() {
                                string.push(' ');
                            }
                            string.push_str(&format!("{byte:02x}"));
                        }
                        string
                    } else {
                        String::from_utf8_lossy(&bytes).to_string()
                    };

                    debug_adapter.rtt_output(handle, data);
                    request.write_status(&mut self.core, 0)?;
                }
            }
            SemihostingCommand::Errno(request) => {
                request.write_errno(&mut self.core, 0)?;
            }

            SemihostingCommand::ExitSuccess => {
                debug_adapter.log_to_console("Application has exited with success.");
                return Ok(CoreStatus::Halted(HaltReason::Breakpoint(
                    BreakpointCause::Semihosting(command),
                )));
            }
            SemihostingCommand::ExitError(details) => {
                debug_adapter.log_to_console(format!("Application has exited with {details}"));
                return Ok(CoreStatus::Halted(HaltReason::Breakpoint(
                    BreakpointCause::Semihosting(command),
                )));
            }

            unhandled => {
                tracing::warn!("Unhandled semihosting command: {:?}", unhandled);
                // Turn unhandled semihosting commands into a breakpoint.
                return Ok(CoreStatus::Halted(HaltReason::Breakpoint(
                    BreakpointCause::Semihosting(unhandled),
                )));
            }
        };

        self.core.run()?;
        Ok(CoreStatus::Running)
    }

    pub fn notify_halted<P: ProtocolAdapter>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
        status: CoreStatus,
    ) -> Result<(), DebuggerError> {
        let program_counter = self.core.read_core_reg(self.core.program_counter()).ok();
        let (reason, description) = status.short_long_status(program_counter);
        let event_body = Some(StoppedEventBody {
            reason: reason.to_string(),
            description: Some(description),
            thread_id: Some(self.id() as i64),
            preserve_focus_hint: Some(false),
            text: None,
            all_threads_stopped: Some(debug_adapter.all_cores_halted),
            hit_breakpoint_ids: None,
        });
        debug_adapter.send_event("stopped", event_body)?;
        tracing::trace!("Notified DAP client that the core halted: {:?}", status);

        Ok(())
    }

    /// Reads memory from the target.
    ///
    /// Returns a vector containing as many bytes as possible to read, stopping at the first error.
    ///
    /// If we can't read any bytes, returns an error.
    pub(crate) fn read_memory_lossy(
        &mut self,
        mut address: u64,
        count: usize,
    ) -> Result<Vec<u8>, Error> {
        let mut num_bytes_unread = count;
        // The probe-rs API does not return partially read data.
        // It either succeeds for the whole buffer or not. However, doing single byte reads is slow, so we will
        // do reads in larger chunks, until we get an error, and then do smaller reads, then smaller, as long as
        // we get any data, to make sure we get all the data we can.
        let mut result_buffer = vec![];

        // Get a suitable chunk size. It needs to be a power of two, at most 256, at most the count.
        fn chunk_size(count: usize, max_chunk_size: usize) -> usize {
            (max_chunk_size.min(count) / 2).next_power_of_two()
        }

        let mut fast_buff = [0u8; 256];
        let mut max_chunk_size = fast_buff.len();

        while num_bytes_unread > 0 && max_chunk_size > 0 {
            let chunk_size = chunk_size(num_bytes_unread, max_chunk_size);
            let buffer = &mut fast_buff[..chunk_size];

            if let Err(e) = self.core.read(address, buffer) {
                // If we haven't read any data yet, and we could not read a single byte, return an error.
                if result_buffer.is_empty() && chunk_size == 1 {
                    return Err(e);
                }

                // Failed to read chunk, try smaller chunk size.
                max_chunk_size = chunk_size / 2;
            } else {
                result_buffer.extend_from_slice(buffer);
                address += chunk_size as u64;
                num_bytes_unread -= chunk_size;
            }
        }

        Ok(result_buffer)
    }

    /// Writes memory of the target core.
    pub(crate) fn write_memory(&mut self, address: u64, data_bytes: &[u8]) -> Result<(), Error> {
        self.core.write_8(address, data_bytes)
    }

    pub(crate) fn reapply_breakpoints(&mut self) {
        if [Architecture::Riscv, Architecture::Xtensa].contains(&self.core.architecture()) {
            let saved_breakpoints = std::mem::take(&mut self.core_data.breakpoints);

            for breakpoint in saved_breakpoints {
                if let Err(error) =
                    self.set_breakpoint(breakpoint.address, breakpoint.breakpoint_type.clone())
                {
                    // This will cause the debugger to show the user an error, but not stop the debugger.
                    tracing::error!(
                        "Failed to re-enable breakpoint {:?} after reset. {}",
                        breakpoint,
                        error
                    );
                }
            }
        }
    }
}

/// Return a Vec of memory ranges that consolidate the adjacent memory ranges of the input ranges.
/// Note: The concept of "adjacent" is calculated to include a gap of up to specified number of bytes between ranges.
/// This serves to consolidate memory ranges that are separated by a small gap, but are still close enough for the purpose of the caller.
fn consolidate_memory_ranges(
    mut discrete_memory_ranges: Vec<Range<u64>>,
    include_bytes_between_ranges: u64,
) -> Vec<Range<u64>> {
    discrete_memory_ranges.sort_by_cached_key(|range| (range.start, range.end));
    discrete_memory_ranges.dedup();
    let mut consolidated_memory_ranges: Vec<Range<u64>> = Vec::new();
    let mut condensed_range: Option<Range<u64>> = None;

    for memory_range in discrete_memory_ranges.iter() {
        if let Some(range_comparator) = condensed_range {
            if memory_range.start <= range_comparator.end + include_bytes_between_ranges + 1 {
                let new_end = std::cmp::max(range_comparator.end, memory_range.end);
                condensed_range = Some(Range {
                    start: range_comparator.start,
                    end: new_end,
                });
            } else {
                consolidated_memory_ranges.push(range_comparator);
                condensed_range = Some(memory_range.clone());
            }
        } else {
            condensed_range = Some(memory_range.clone());
        }
    }

    if let Some(range_comparator) = condensed_range {
        consolidated_memory_ranges.push(range_comparator);
    }

    consolidated_memory_ranges
}

/// A single range should remain the same after consolidation.
#[test]
fn test_single_range() {
    let input = vec![Range { start: 0, end: 5 }];
    let expected = vec![Range { start: 0, end: 5 }];
    let result = consolidate_memory_ranges(input, 0);
    assert_eq!(result, expected);
}

/// Three ranges that are adjacent should be consolidated into one.
#[test]
fn test_three_adjacent_ranges() {
    let input = vec![
        Range { start: 0, end: 5 },
        Range { start: 6, end: 10 },
        Range { start: 11, end: 15 },
    ];
    let expected = vec![Range { start: 0, end: 15 }];
    let result = consolidate_memory_ranges(input, 0);
    assert_eq!(result, expected);
}

/// Two ranges that are distinct should remain distinct after consolidation.
#[test]
fn test_distinct_ranges() {
    let input = vec![Range { start: 0, end: 5 }, Range { start: 7, end: 10 }];
    let expected = vec![Range { start: 0, end: 5 }, Range { start: 7, end: 10 }];
    let result = consolidate_memory_ranges(input, 0);
    assert_eq!(result, expected);
}

/// Two ranges that are contiguous should be consolidated into one.
#[test]
fn test_contiguous_ranges() {
    let input = vec![Range { start: 0, end: 5 }, Range { start: 5, end: 10 }];
    let expected = vec![Range { start: 0, end: 10 }];
    let result = consolidate_memory_ranges(input, 0);
    assert_eq!(result, expected);
}

/// Three ranges where the first two are adjacent and the third is distinct should be consolidated into two.
#[test]
fn test_adjacent_and_distinct_ranges() {
    let input = vec![
        Range { start: 0, end: 5 },
        Range { start: 6, end: 10 },
        Range { start: 12, end: 15 },
    ];
    let expected = vec![Range { start: 0, end: 10 }, Range { start: 12, end: 15 }];
    let result = consolidate_memory_ranges(input, 0);
    assert_eq!(result, expected);
}

/// Two ranges where the second starts and ends before the first should remain distinct after consolidation.
#[test]
fn test_non_overlapping_ranges() {
    let input = vec![Range { start: 10, end: 20 }, Range { start: 0, end: 5 }];
    let expected = vec![Range { start: 0, end: 5 }, Range { start: 10, end: 20 }];
    let result = consolidate_memory_ranges(input, 0);
    assert_eq!(result, expected);
}

/// Two ranges where the second starts and ends before the first but are consolidated because they are within 5 bytes of each other.
#[test]
fn test_non_overlapping_ranges_with_extra_bytes() {
    let input = vec![Range { start: 10, end: 20 }, Range { start: 0, end: 5 }];
    let expected = vec![Range { start: 0, end: 20 }];
    let result = consolidate_memory_ranges(input, 5);
    assert_eq!(result, expected);
}

/// Two ranges where the second starts before, but intersects with the first, should be consolidated.
#[test]
fn test_reversed_intersecting_ranges() {
    let input = vec![Range { start: 10, end: 20 }, Range { start: 5, end: 15 }];
    let expected = vec![Range { start: 5, end: 20 }];
    let result = consolidate_memory_ranges(input, 0);
    assert_eq!(result, expected);
}
