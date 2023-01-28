use super::session_data;
use crate::{
    debug_adapter::{
        dap_adapter::{DapStatus, DebugAdapter},
        dap_types::{ContinuedEventBody, MessageSeverity, StoppedEventBody},
        protocol::ProtocolAdapter,
    },
    debugger::debug_rtt,
    peripherals::svd_variables::SvdCache,
    DebuggerError,
};
use anyhow::{anyhow, Result};
use probe_rs::{debug::debug_info::DebugInfo, Core, CoreStatus, Error};
use probe_rs_cli_util::rtt::{self, ChannelMode, DataFormat};

/// [CoreData] is used to cache data needed by the debugger, on a per-core basis.
pub struct CoreData {
    pub(crate) core_index: usize,
    /// Track the last_known_status of the core.
    /// The debug client needs to be notified when the core changes state, and this can happen in one of two ways:
    /// 1. By polling the core status periodically (in [`super::debug_entry::Debugger::process_next_request()`]).
    ///   For instance, when the client sets the core running, and the core halts because of a breakpoint, we need to notify the client.
    /// 2. Some requests, like [`DebugAdapter::next()`], has an implicit action of setting the core running, before it waits for it to halt at the next statement.
    ///   To ensure the [`CoreHandle::poll_core()`] behaves correctly, it will set the `last_known_status` to [`CoreStatus::Running`],
    ///   and execute the request normally, with the expectation that the core will be halted, and that 1. above will detect this new status.
    ///   These 'implicit' updates of `last_known_status` will not(and should not) result in a notification to the client.
    pub(crate) last_known_status: CoreStatus,
    pub(crate) target_name: String,
    pub(crate) debug_info: DebugInfo,
    pub(crate) core_peripherals: Option<SvdCache>,
    pub(crate) stack_frames: Vec<probe_rs::debug::stack_frame::StackFrame>,
    pub(crate) breakpoints: Vec<session_data::ActiveBreakpoint>,
    pub(crate) rtt_connection: Option<debug_rtt::RttConnection>,
}

/// [CoreHandle] provides handles to various data structures required to debug a single instance of a core. The actual state is stored in [session_data::SessionData].
///
/// Usage: To get access to this structure please use the [session_data::SessionData::attach_core] method. Please keep access/locks to this to a minumum duration.
pub struct CoreHandle<'p> {
    pub(crate) core: Core<'p>,
    pub(crate) core_data: &'p mut CoreData,
}

impl<'p> CoreHandle<'p> {
    /// Some MS DAP requests (e.g. `step`) implicitly expect the core to resume processing and then to optionally halt again, before the request completes.
    ///
    /// This method is used to set the `last_known_status` to [`CoreStatus::Unknown`] (because we cannot verify that it will indeed resume running until we have polled it again),
    ///   as well as [`DebugAdapter::all_cores_halted`] = `false`, without notifying the client of any status changes.
    pub(crate) fn reset_core_status<P: ProtocolAdapter>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
    ) {
        self.core_data.last_known_status = CoreStatus::Running;
        debug_adapter.all_cores_halted = false;
    }

    /// - Whenever we check the status, we compare it against `last_known_status` and send the appropriate event to the client.
    /// - If we cannot determine the core status, then there is no sense in continuing the debug session, so please propogate the error.
    /// - If the core status has changed, then we update `last_known_status` to the new value, and return `true` as part of the Result<>.
    pub(crate) fn poll_core<P: ProtocolAdapter>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
    ) -> Result<CoreStatus, Error> {
        if debug_adapter.configuration_is_done() {
            match self.core.status() {
                Ok(status) => {
                    let has_changed_state = status != self.core_data.last_known_status;
                    if has_changed_state {
                        match status {
                            CoreStatus::Running | CoreStatus::Sleeping => {
                                let event_body = Some(ContinuedEventBody {
                                    all_threads_continued: Some(true), // TODO: Implement multi-core awareness here
                                    thread_id: self.core.id() as i64,
                                });
                                debug_adapter.send_event("continued", event_body)?;
                                tracing::trace!(
                                    "Notified DAP client that the core continued: {:?}",
                                    status
                                );
                            }
                            CoreStatus::Halted(_) => {
                                let program_counter = self
                                    .core
                                    .read_core_reg(self.core.registers().program_counter())
                                    .ok();
                                let event_body = Some(StoppedEventBody {
                                    reason: status.short_long_status(program_counter).0.to_owned(),
                                    description: Some(status.short_long_status(program_counter).1),
                                    thread_id: Some(self.core.id() as i64),
                                    preserve_focus_hint: Some(false),
                                    text: None,
                                    all_threads_stopped: Some(debug_adapter.all_cores_halted),
                                    hit_breakpoint_ids: None,
                                });
                                debug_adapter.send_event("stopped", event_body)?;
                                tracing::trace!(
                                    "Notified DAP client that the core halted: {:?}",
                                    status
                                );
                            }
                            CoreStatus::LockedUp => {
                                debug_adapter.show_message(
                                    MessageSeverity::Error,
                                    status.short_long_status(None).1,
                                );
                                return Err(Error::Other(anyhow!(
                                    status.short_long_status(None).1
                                )));
                            }
                            CoreStatus::Unknown => {
                                debug_adapter.send_error_response(&DebuggerError::Other(
                                    anyhow!("Unknown Device status reveived from Probe-rs"),
                                ))?;

                                return Err(Error::Other(anyhow!(
                                    "Unknown Device status reveived from Probe-rs"
                                )));
                            }
                        }
                    }
                    self.core_data.last_known_status = status; // Update this unconditionally, because halted() can have more than one variant.
                    Ok(status)
                }
                Err(error) => {
                    self.core_data.last_known_status = CoreStatus::Unknown;
                    Err(error)
                }
            }
        } else {
            tracing::trace!(
                "Ignored last_known_status: {:?} during `configuration_done=false`, and reset it to {:?}.",
                self.core_data.last_known_status,
                CoreStatus::Unknown
            );
            Ok(CoreStatus::Unknown)
        }
    }

    /// Search available [`probe_rs::debug::StackFrame`]'s for the given `id`
    pub(crate) fn get_stackframe(
        &'p self,
        id: i64,
    ) -> Option<&'p probe_rs::debug::stack_frame::StackFrame> {
        self.core_data
            .stack_frames
            .iter()
            .find(|stack_frame| stack_frame.id == id)
    }

    /// Confirm RTT initialization on the target, and use the RTT channel configurations to initialize the output windows on the DAP Client.
    pub fn attach_to_rtt<P: ProtocolAdapter>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
        target_memory_map: &[probe_rs::config::MemoryRegion],
        program_binary: &std::path::Path,
        rtt_config: &rtt::RttConfig,
    ) -> Result<()> {
        let mut debugger_rtt_channels: Vec<debug_rtt::DebuggerRttChannel> = vec![];
        match rtt::attach_to_rtt(
            &mut self.core,
            target_memory_map,
            program_binary,
            rtt_config,
        ) {
            Ok(target_rtt) => {
                for any_channel in target_rtt.active_channels.iter() {
                    if let Some(up_channel) = &any_channel.up_channel {
                        if any_channel.data_format == DataFormat::Defmt {
                            // For defmt, we set the channel to be blocking when full.
                            up_channel.set_mode(&mut self.core, ChannelMode::BlockIfFull)?;
                        }
                        debugger_rtt_channels.push(debug_rtt::DebuggerRttChannel {
                            channel_number: up_channel.number(),
                            // This value will eventually be set to true by a VSCode client request "rttWindowOpened"
                            has_client_window: false,
                        });
                        debug_adapter.rtt_window(
                            up_channel.number(),
                            any_channel.channel_name.clone(),
                            any_channel.data_format,
                        );
                    }
                }
                self.core_data.rtt_connection = Some(debug_rtt::RttConnection {
                    target_rtt,
                    debugger_rtt_channels,
                });
            }
            Err(_error) => {
                tracing::warn!("Failed to initalize RTT. Will try again on the next request... ");
            }
        };
        Ok(())
    }

    /// Set a single breakpoint in target configuration as well as [`super::core_data::CoreHandle`]
    pub(crate) fn set_breakpoint(
        &mut self,
        address: u64,
        breakpoint_type: session_data::BreakpointType,
    ) -> Result<(), DebuggerError> {
        self.core
            .set_hw_breakpoint(address)
            .map_err(DebuggerError::ProbeRs)?;
        self.core_data
            .breakpoints
            .push(session_data::ActiveBreakpoint {
                breakpoint_type,
                breakpoint_address: address,
            });
        Ok(())
    }

    /// Clear a single breakpoint from target configuration.
    pub(crate) fn clear_breakpoint(&mut self, address: u64) -> Result<()> {
        self.core
            .clear_hw_breakpoint(address)
            .map_err(DebuggerError::ProbeRs)?;
        let mut breakpoint_position: Option<usize> = None;
        for (position, active_breakpoint) in self.core_data.breakpoints.iter().enumerate() {
            if active_breakpoint.breakpoint_address == address {
                breakpoint_position = Some(position);
                break;
            }
        }
        if let Some(breakpoint_position) = breakpoint_position {
            self.core_data.breakpoints.remove(breakpoint_position);
        }
        Ok(())
    }

    /// Clear all breakpoints of a specified [`super::session_data::BreakpointType`]. Affects target configuration as well as [`super::core_data::CoreHandle`]
    pub(crate) fn clear_breakpoints(
        &mut self,
        breakpoint_type: session_data::BreakpointType,
    ) -> Result<()> {
        let target_breakpoints = self
            .core_data
            .breakpoints
            .iter()
            .filter(|breakpoint| breakpoint.breakpoint_type == breakpoint_type)
            .map(|breakpoint| breakpoint.breakpoint_address)
            .collect::<Vec<u64>>();
        for breakpoint in target_breakpoints {
            self.clear_breakpoint(breakpoint).ok();
        }
        Ok(())
    }
}
