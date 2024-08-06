use std::{ops::Range, path::Path};

use super::session_data::{self, ActiveBreakpoint, BreakpointType, SourceLocationScope};
use crate::util::rtt::{self, DataFormat, DefmtState, RttActiveTarget};
use crate::{
    cmd::dap_server::{
        debug_adapter::{
            dap::{
                adapter::DebugAdapter,
                core_status::DapStatus,
                dap_types::{ContinuedEventBody, MessageSeverity, Source, StoppedEventBody},
            },
            protocol::ProtocolAdapter,
        },
        peripherals::svd_variables::SvdCache,
        server::debug_rtt,
        DebuggerError,
    },
    util::rtt::RttConfig,
};
use anyhow::{anyhow, Result};
use probe_rs::debug::VerifiedBreakpoint;
use probe_rs::{
    debug::{
        debug_info::DebugInfo, stack_frame::StackFrameInfo, ColumnType, ObjectRef, VariableCache,
    },
    rtt::{Rtt, ScanRegion},
    Core, CoreStatus, HaltReason,
};
use time::UtcOffset;
use typed_path::TypedPathBuf;

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
    pub debug_info: DebugInfo,
    pub static_variables: Option<VariableCache>,
    pub core_peripherals: Option<SvdCache>,
    pub stack_frames: Vec<probe_rs::debug::stack_frame::StackFrame>,
    pub breakpoints: Vec<session_data::ActiveBreakpoint>,
    pub rtt_connection: Option<debug_rtt::RttConnection>,
}

/// [CoreHandle] provides handles to various data structures required to debug a single instance of a core. The actual state is stored in [session_data::SessionData].
///
/// Usage: To get access to this structure please use the [session_data::SessionData::attach_core] method. Please keep access/locks to this to a minimum duration.
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
    /// - If we cannot determine the core status, then there is no sense in continuing the debug session, so please propagate the error.
    /// - If the core status has changed, then we update `last_known_status` to the new value, and return `true` as part of the Result<>.
    pub(crate) fn poll_core<P: ProtocolAdapter>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
    ) -> Result<CoreStatus, DebuggerError> {
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
                                // HaltReason::Step is a special case, where we have to send a custome event to the client that the core halted.
                                // In this case, we don't re-send the "stopped" event, but further down, we will
                                // update the `last_known_status` to the actual HaltReason returned by the core.
                                if self.core_data.last_known_status
                                    != CoreStatus::Halted(HaltReason::Step)
                                {
                                    let program_counter =
                                        self.core.read_core_reg(self.core.program_counter()).ok();
                                    let event_body = Some(StoppedEventBody {
                                        reason: status
                                            .short_long_status(program_counter)
                                            .0
                                            .to_owned(),
                                        description: Some(
                                            status.short_long_status(program_counter).1,
                                        ),
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
                            }
                            CoreStatus::LockedUp => {
                                debug_adapter.show_message(
                                    MessageSeverity::Error,
                                    status.short_long_status(None).1,
                                );
                                return Err(DebuggerError::Other(anyhow!(
                                    status.short_long_status(None).1
                                )));
                            }
                            CoreStatus::Unknown => {
                                debug_adapter.show_error_message(&DebuggerError::Other(
                                    anyhow!("Unknown Device status reveived from Probe-rs"),
                                ))?;

                                return Err(DebuggerError::Other(anyhow!(
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
                    Err(error.into())
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
        id: ObjectRef,
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
        program_binary: &Path,
        rtt_config: &rtt::RttConfig,
        timestamp_offset: UtcOffset,
    ) -> Result<()> {
        let mut debugger_rtt_channels: Vec<debug_rtt::DebuggerRttChannel> = vec![];

        let elf = std::fs::read(program_binary)
            .map_err(|error| anyhow!("Error attempting to attach to RTT: {error}"))?;

        let defmt_state = DefmtState::try_from_bytes(&elf)
            .map_err(|err| anyhow!("Failed to process defmt data: {err}"))?;

        // Attach to RTT by using the RTT control block address from the ELF file. Do not scan the memory for the control block.
        let Ok(target_rtt) = try_attach_rtt(
            &mut self.core,
            &elf,
            rtt_config,
            timestamp_offset,
            defmt_state.as_ref(),
        ) else {
            tracing::warn!("Failed to initalize RTT. Will try again on the next request... ");
            return Ok(());
        };

        for up_channel in target_rtt.active_up_channels.iter() {
            debugger_rtt_channels.push(debug_rtt::DebuggerRttChannel {
                channel_number: up_channel.number(),
                // This value will eventually be set to true by a VSCode client request "rttWindowOpened"
                has_client_window: false,
            });
            debug_adapter.rtt_window(
                up_channel.number(),
                up_channel.channel_name.clone(),
                DataFormat::from(&up_channel.data_format),
            );
        }

        self.core_data.rtt_connection = Some(debug_rtt::RttConnection {
            target_rtt,
            debugger_rtt_channels,
            defmt_state,
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
    pub(crate) fn clear_breakpoint(&mut self, address: u64) -> Result<()> {
        self.core
            .clear_hw_breakpoint(address)
            .map_err(DebuggerError::ProbeRs)?;
        if let Some((breakpoint_position, _)) = self.find_breakpoint_in_cache(address) {
            self.core_data.breakpoints.remove(breakpoint_position);
        }
        Ok(())
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
        source_path: &TypedPathBuf,
        requested_breakpoint_line: u64,
        requested_breakpoint_column: Option<u64>,
        requested_source: &Source,
    ) -> Result<VerifiedBreakpoint, DebuggerError> {
        let VerifiedBreakpoint {
                 address,
                 source_location,
             } = self.core_data
            .debug_info
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
                source: requested_source.clone(),
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
                let breakpoint_err = source_location
                    .combined_typed_path()
                    .as_ref()
                    .ok_or_else(|| DebuggerError::Other(anyhow!("Unable to get source location")))
                    .and_then(|requested_path| {
                        self.verify_and_set_breakpoint(
                            requested_path,
                            source_location.line.unwrap_or(0),
                            source_location.column.map(|col| match col {
                                ColumnType::LeftEdge => 0_u64,
                                ColumnType::Column(c) => c,
                            }),
                            &source,
                        )
                    });

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

        if let Some(static_variables) = &mut self.core_data.static_variables {
            static_variables.recurse_deferred_variables(
                &self.core_data.debug_info,
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
                // Cache the deferred top level children of the of the cache.
                variable_cache.recurse_deferred_variables(
                    &self.core_data.debug_info,
                    &mut self.core,
                    10,
                    StackFrameInfo {
                        registers: &frame.registers,
                        frame_base: frame.frame_base,
                        canonical_frame_address: frame.canonical_frame_address,
                    },
                );
                all_discrete_memory_ranges.append(&mut variable_cache.get_discrete_memory_ranges());
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
}

fn try_attach_rtt(
    core: &mut Core,
    elf: &[u8],
    rtt_config: &RttConfig,
    timestamp_offset: UtcOffset,
    defmt_state: Option<&DefmtState>,
) -> Result<RttActiveTarget, DebuggerError> {
    let header_address = RttActiveTarget::get_rtt_symbol_from_bytes(&elf)
        .ok_or_else(|| anyhow!("No RTT control block found in ELF file"))?;

    let scan_region = ScanRegion::Exact(header_address);

    let rtt = Rtt::attach_region(core, &scan_region)
        .map_err(|error| anyhow!("Error attempting to attach to RTT: {error}"))?;

    tracing::info!("RTT initialized.");
    let target = RttActiveTarget::new(core, rtt, defmt_state, rtt_config, timestamp_offset)
        .map_err(|err| anyhow!("Failed to attach to RTT: {err}"))?;

    Ok(target)
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
        if let Some(range_comparitor) = condensed_range {
            if memory_range.start <= range_comparitor.end + include_bytes_between_ranges + 1 {
                let new_end = std::cmp::max(range_comparitor.end, memory_range.end);
                condensed_range = Some(Range {
                    start: range_comparitor.start,
                    end: new_end,
                });
            } else {
                consolidated_memory_ranges.push(range_comparitor);
                condensed_range = Some(memory_range.clone());
            }
        } else {
            condensed_range = Some(memory_range.clone());
        }
    }

    if let Some(range_comparitor) = condensed_range {
        consolidated_memory_ranges.push(range_comparitor);
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
