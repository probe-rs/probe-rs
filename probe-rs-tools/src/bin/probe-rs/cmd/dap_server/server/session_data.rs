use super::{
    configuration::{self, CoreConfig, SessionConfig},
    core_data::{ChannelNames, CoreData},
};
use crate::cmd::dap_server::debug_adapter::dap::dap_types::PromptKind;
use crate::cmd::dap_server::server::debug_rtt;
use crate::cmd::{
    dap_server::{
        DebuggerError,
        backend::rpc::{CorePerAttachInfo, RpcBackend, SessionTargetMetadata, rpc_err},
        debug_adapter::dap::{
            adapter::DebugAdapter,
            core_status::DapStatus,
            dap_types::{ContinuedEventBody, MessageSeverity, Source, StoppedEventBody},
            repl_commands::{REPL_COMMANDS, embedded_test::EMBEDDED_TEST},
        },
    },
    run::EmbeddedTestElfInfo,
};
use crate::rpc::{Key, RttClient};
use crate::util::cli::attach_probe as attach_probe_rpc;
use crate::util::rtt::{DefmtProcessor, DefmtState, RttDecoder};
use anyhow::{Result, anyhow};
use probe_rs::{BreakpointCause, CoreStatus, HaltReason, rtt::find_rtt_control_block_in_raw_file};
use probe_rs_debug::SourceLocation;
use probe_rs_rpc::breakpoints::SourceBreakpointLocation;
use probe_rs_rpc::format::FormatKind;
use probe_rs_rpc::rtt_client::ScanRegion as WireScanRegion;
use probe_rs_rpc_client::{ResolvedUpload, RpcClient, SessionInterface};
use std::{any::Any, env::set_current_dir, path::Path};
use time::UtcOffset;

use crate::util::rtt::RttConfig;
use probe_rs_rpc::rtt_config::DataFormat;

/// The supported breakpoint types
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BreakpointType {
    /// A breakpoint was requested using an instruction address, and usually a result of a user requesting a
    /// breakpoint while in a 'disassembly' view.
    InstructionBreakpoint,
    /// A breakpoint that has a Source, and usually a result of a user requesting a breakpoint while in a 'source' view.
    SourceBreakpoint {
        source: Box<Source>,
        location: SourceLocationScope,
    },
}

/// Breakpoint requests refer to a specific `SourceLocation` for a `Source`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SourceLocationScope {
    Specific(SourceLocation),
}

/// Provide the storage and methods to handle various [`BreakpointType`]
#[derive(Clone, Debug)]
pub struct ActiveBreakpoint {
    pub(crate) breakpoint_type: BreakpointType,
    pub(crate) address: u64,
}

/// DAP-session state and per-core display metadata.
///
/// Target access is owned by the RPC server. [`SessionData`] routes core
/// operations through [`RpcBackend`] and keeps one [`CoreData`] record for
/// each configured core; it does not retain a client-side core handle.
/// TODO: Adjust [SessionConfig] to allow multiple cores (and if appropriate, their binaries) to be specified.
///
/// The DAP server drives the target through an [`RpcBackend`]: even local
/// sessions run through the in-process RPC server, so the debugger never
/// touches a [`probe_rs::Session`] directly.
pub(crate) struct SessionData {
    pub(crate) backend: RpcBackend,
    /// [SessionData] will manage one [CoreData] per target core, that is also present in [SessionConfig::core_configs]
    pub(crate) core_data: Vec<CoreData>,

    /// Offset used for RTC timestamps
    ///
    /// Getting the offset can fail, so it's better to store it.
    timestamp_offset: UtcOffset,
}

impl SessionData {
    /// Build a [`SessionData`] backed by an [`RpcBackend`] against the
    /// supplied [`RpcClient`].
    ///
    /// The server is expected to have its chip registry populated (either
    /// through the builtin database or through a prior
    /// `client.load_chip_family`). The `config.chip` field is required: it
    /// drives the remote `probe/attach` RPC. Session-scoped target facts
    /// (name, default image format, core list) are fetched from the server
    /// after attach so the DAP client does not mirror the full target
    /// description locally.
    ///
    /// When `preattached` is `Some`, that session is reused and `probe/attach`
    /// is not called again.
    pub(crate) async fn new_rpc_backed(
        client: &RpcClient,
        config: &mut configuration::SessionConfig,
        timestamp_offset: UtcOffset,
        preattached: Option<SessionInterface>,
    ) -> Result<Self, DebuggerError> {
        let session = match preattached {
            Some(session) => session,
            None => {
                // Reuse the shared CLI helper: it uploads any user-supplied chip
                // description, selects a probe, and performs the `probe/attach` RPC.
                let probe_options = config.probe_options();
                attach_probe_rpc(client, probe_options, None, false).await?
            }
        };
        let sessid = session.session_key();

        let wire_metadata = session.target_metadata().await.map_err(|e| {
            DebuggerError::Other(anyhow!(
                "Failed to fetch target metadata from RPC server: {e}"
            ))
        })?;

        let target_metadata = SessionTargetMetadata::from(wire_metadata);

        apply_session_cwd(config)?;

        let cores = target_metadata.cores.clone();

        // `probe/attach` leaves the target halted, so query the live core
        // properties needed to select the correct static register file. Only
        // the core being debugged is queried: the debugger attaches to a single
        // core, and a core that firmware has not enabled (such as the second
        // core of an ESP32-S3) cannot be queried without failing the attach.
        let mut per_core = Vec::with_capacity(cores.len());
        for (core_index, core_type) in &cores {
            let architecture = core_type.architecture();
            let is_debugged = config
                .core_configs
                .iter()
                .any(|core_config| core_config.core_index == *core_index);

            per_core.push(if is_debugged {
                CorePerAttachInfo::query(client, sessid, *core_index, architecture).await?
            } else {
                CorePerAttachInfo::not_debugged(architecture)
            });
        }

        let mut backend = RpcBackend::new(client.clone(), sessid, target_metadata, per_core);

        let core_data_vec = initialize_core_data(&mut backend, config)?;
        for core_config in config.core_configs.iter() {
            backend
                .apply_vector_catch(core_config.core_index, core_config)
                .await?;
        }

        // Eagerly populate the authoritative server-side `DebugInfo` so
        // consumers can resolve source locations before the first halt. Use
        // the first configured core's binary: the accepted single-core model
        // still caches one `DebugInfo` per session (multi-core is deferred).
        if let Some(Some(path)) = config
            .core_configs
            .first()
            .map(|c| c.program_binary.as_deref())
        {
            backend
                .session_interface()
                .load_debug_info(path.to_path_buf())
                .await
                .map_err(|e| {
                    DebuggerError::Other(anyhow::anyhow!("Failed to load debug info: {e}"))
                })?;
        }

        let mut this = SessionData {
            backend,
            core_data: core_data_vec,
            timestamp_offset,
        };

        this.load_rtt_location(config)?;

        Ok(this)
    }

    pub(crate) fn core_data_opt(&self, core_index: usize) -> Option<&CoreData> {
        self.core_data.iter().find(|cd| cd.core_index == core_index)
    }

    pub(crate) fn core_data(&self, core_index: usize) -> Result<&CoreData, DebuggerError> {
        self.core_data
            .iter()
            .find(|cd| cd.core_index == core_index)
            .ok_or_else(|| DebuggerError::Other(anyhow!("No core data for core {core_index}")))
    }

    pub(crate) fn core_data_mut(
        &mut self,
        core_index: usize,
    ) -> Result<&mut CoreData, DebuggerError> {
        self.core_data
            .iter_mut()
            .find(|cd| cd.core_index == core_index)
            .ok_or_else(|| DebuggerError::Other(anyhow!("No core data for core {core_index}")))
    }

    pub(crate) fn load_rtt_location(
        &mut self,
        config: &configuration::SessionConfig,
    ) -> Result<(), DebuggerError> {
        // Filter `CoreConfig` entries based on those that match an actual core on the target probe.
        let valid_core_configs = config.core_configs.iter().filter(|&core_config| {
            self.backend
                .target_metadata
                .cores
                .iter()
                .any(|(target_core_index, _)| *target_core_index == core_config.core_index)
        });

        let image_format = config
            .flashing_config
            .format_options
            .binary_format
            .resolve_default_format(self.backend.target_metadata.default_format.as_deref());

        for core_configuration in valid_core_configs {
            let Some(core_data) = self
                .core_data
                .iter_mut()
                .find(|core_data| core_data.core_index == core_configuration.core_index)
            else {
                continue;
            };

            core_data.defmt_state = None;
            core_data.rtt_scan_ranges = match core_configuration.program_binary.as_ref() {
                Some(program_binary)
                    if matches!(image_format, FormatKind::Elf | FormatKind::Idf) =>
                {
                    let elf = std::fs::read(program_binary)
                        .map_err(|error| anyhow!("Error attempting to attach to RTT: {error}"))?;

                    if let Ok(Some(addr)) = find_rtt_control_block_in_raw_file(&elf) {
                        WireScanRegion::Exact(addr)
                    } else {
                        // Do not scan the memory for the control block.
                        WireScanRegion::Ranges(vec![])
                    }
                }
                _ => WireScanRegion::Ranges(vec![]),
            };
        }

        Ok(())
    }

    /// Return the server-side `RttClient` handle for a core, creating it on
    /// first use and caching it in [`CoreData::rtt_remote_handle`].
    ///
    /// Creating the client touches neither the probe nor the target, so this
    /// is safe to call before the RTT link has been established.
    async fn ensure_rtt_client(
        &mut self,
        cd_idx: usize,
        rtt_config: &RttConfig,
    ) -> Result<Key<RttClient>> {
        if let Some(handle) = self.core_data[cd_idx].rtt_remote_handle {
            return Ok(handle);
        }

        let wire_scan = self.core_data[cd_idx].rtt_scan_ranges.clone();

        let data = self
            .backend
            .session_interface()
            .create_rtt_client(
                wire_scan,
                rtt_config.channels.clone(),
                rtt_config.default_config.clone(),
            )
            .await
            .map_err(|e| anyhow!("Failed to create remote RTT client: {e}"))?;

        self.core_data[cd_idx].rtt_remote_handle = Some(data.handle);
        Ok(data.handle)
    }

    /// The first RTT-enabled core configuration that matches a core of the
    /// target, together with the server-side `RttClient` of that core.
    async fn rtt_core<'config>(
        &mut self,
        config: &'config SessionConfig,
    ) -> Result<Option<(&'config CoreConfig, Key<RttClient>)>, DebuggerError> {
        for core_config in config.core_configs.iter() {
            if !core_config.rtt_config.enabled {
                continue;
            }
            let Some(cd_idx) = self
                .core_data
                .iter()
                .position(|c| c.core_index == core_config.core_index)
            else {
                continue;
            };

            let rtt_key = self
                .ensure_rtt_client(cd_idx, &core_config.rtt_config)
                .await?;

            return Ok(Some((core_config, rtt_key)));
        }

        Ok(None)
    }

    /// The server-side `RttClient` of the debugged core, or `None` when RTT is
    /// disabled.
    pub(crate) async fn rtt_client(
        &mut self,
        config: &SessionConfig,
    ) -> Result<Option<Key<RttClient>>, DebuggerError> {
        Ok(self.rtt_core(config).await?.map(|(_, rtt_key)| rtt_key))
    }

    /// Tell the RTT client which program the target holds, so that it knows
    /// whether the download writes the control block.
    ///
    /// The image, not the download, carries this information, thus this runs
    /// for a program that another tool flashed as well.
    pub(crate) async fn configure_rtt_from_image(
        &mut self,
        config: &SessionConfig,
    ) -> Result<(), DebuggerError> {
        let Some((core_config, rtt_key)) = self.rtt_core(config).await? else {
            return Ok(());
        };
        let Some(program_binary) = core_config.program_binary.clone() else {
            return Ok(());
        };

        // An image that probe-rs cannot load also cannot have written the
        // control block, thus a failure here leaves clearing enabled.
        if let Err(error) = self
            .backend
            .session_interface()
            .build_flash_loader(
                program_binary,
                config.flashing_config.format_options.clone(),
                None,
                false,
                Some(rtt_key),
            )
            .await
        {
            tracing::warn!("Failed to inspect the program binary for RTT: {error}");
        }

        Ok(())
    }

    /// Clear the stale RTT control block of the debugged core.
    ///
    /// This should be called while the core is halted, before a reset, to wipe
    /// stale RTT data from a previous debug session. After reset, the firmware
    /// startup code will reinitialize the block from `.data`.
    ///
    /// The clear is routed through the server-side `RttClient`, so a control
    /// block that the flash loader initializes in RAM is left alone.
    pub(crate) async fn clear_rtt_block(
        &mut self,
        config: &SessionConfig,
    ) -> Result<(), DebuggerError> {
        let Some(rtt_key) = self.rtt_client(config).await? else {
            return Ok(());
        };

        self.backend
            .session_interface()
            .clear_rtt_control_block(rtt_key)
            .await
            .map_err(rpc_err)
            .map_err(DebuggerError::ProbeRs)
    }

    /// Recompute source breakpoint addresses after a restart that flashed a
    /// new binary: re-verify each cached source breakpoint against the
    /// (reloaded) debug info and re-set the hardware breakpoints, since the
    /// address of a source location may have shifted.
    pub(crate) async fn recompute_breakpoints(
        &mut self,
        core_index: usize,
    ) -> Result<(), DebuggerError> {
        let (old_addrs, pending) = {
            let Some(core_data) = self.core_data_opt(core_index) else {
                return Ok(());
            };
            let mut old_addrs: Vec<u64> = Vec::new();
            let mut pending: Vec<(Box<Source>, SourceLocation)> = Vec::new();
            for bp in &core_data.breakpoints {
                let BreakpointType::SourceBreakpoint {
                    source,
                    location: SourceLocationScope::Specific(loc),
                } = &bp.breakpoint_type
                else {
                    continue;
                };
                old_addrs.push(bp.address);
                pending.push((source.clone(), loc.clone()));
            }
            (old_addrs, pending)
        };
        if old_addrs.is_empty() {
            return Ok(());
        }
        let requests = pending
            .iter()
            .map(|(_, location)| SourceBreakpointLocation {
                path: location.path.to_path().display().to_string(),
                line: location.line.unwrap_or(0),
                column: location.column.map(|column| match column {
                    probe_rs_debug::ColumnType::LeftEdge => 0,
                    probe_rs_debug::ColumnType::Column(column) => column,
                }),
            })
            .collect();
        let resolved = self
            .backend
            .resolve_source_breakpoints(requests)
            .await
            .map_err(DebuggerError::ProbeRs)?;
        let mut to_set = Vec::with_capacity(resolved.len());
        for ((source, location), result) in pending.into_iter().zip(resolved) {
            match result {
                Ok(verified) => to_set.push((verified.address, source, verified.source_location)),
                Err(error) => {
                    return Err(DebuggerError::Other(anyhow!(
                        "Failed to recompute breakpoint at {location:?} in {source:?}. Error: {error}"
                    )));
                }
            }
        }
        self.backend
            .clear_hw_breakpoints(core_index, old_addrs)
            .await
            .map_err(DebuggerError::ProbeRs)?;
        if let Ok(core_data) = self.core_data_mut(core_index) {
            core_data.breakpoints.retain(|bp| {
                !matches!(&bp.breakpoint_type, BreakpointType::SourceBreakpoint { .. })
            });
        }
        let set_addrs: Vec<u64> = to_set.iter().map(|(a, _, _)| *a).collect();
        let set_results = self
            .backend
            .set_hw_breakpoints(core_index, set_addrs)
            .await
            .map_err(DebuggerError::ProbeRs)?;
        if let Ok(core_data) = self.core_data_mut(core_index) {
            for (i, (addr, source, loc)) in to_set.into_iter().enumerate() {
                if set_results.get(i).is_some_and(|result| result.is_ok()) {
                    core_data.breakpoints.push(ActiveBreakpoint {
                        breakpoint_type: BreakpointType::SourceBreakpoint {
                            source,
                            location: SourceLocationScope::Specific(loc),
                        },
                        address: addr,
                    });
                }
            }
        }
        Ok(())
    }

    /// Publish server-owned debug info from a prior [`ResolvedUpload`].
    pub(crate) async fn reload_debug_info_resolved(
        &mut self,
        core_configuration: &CoreConfig,
        upload: &ResolvedUpload,
    ) -> Result<(), DebuggerError> {
        let Some(core_data_index) = self
            .core_data
            .iter()
            .position(|core_data| core_data.core_index == core_configuration.core_index)
        else {
            return Err(DebuggerError::UnableToOpenProbe(Some(
                "No core at the specified index.",
            )));
        };

        self.backend
            .session_interface()
            .load_debug_info_resolved(upload)
            .await
            .map_err(|error| {
                DebuggerError::Other(anyhow!("Failed to reload server debug info: {error}"))
            })?;

        let core_data = &mut self.core_data[core_data_index];
        core_data.invalidate_stack_frame_cache();
        Ok(())
    }

    /// Emit the `stopped` event for a halted core without a live `Core`: the
    /// PC is read via `backend.program_counter_id` + `backend.read_core_reg`
    /// (one round trip for the register read; the PC id is cached).
    async fn notify_halted(
        &mut self,
        debug_adapter: &mut DebugAdapter,
        cd_idx: usize,
        status: CoreStatus,
    ) -> Result<(), DebuggerError> {
        let core_index = self.core_data[cd_idx].core_index;
        let program_counter = self.backend.program_counter(core_index).await;
        let (reason, description) = status.short_long_status(program_counter);
        let event_body = Some(StoppedEventBody {
            reason: reason.to_string(),
            description: Some(description),
            thread_id: Some(core_index as i64),
            preserve_focus_hint: Some(false),
            text: None,
            all_threads_stopped: Some(debug_adapter.all_cores_halted),
            hit_breakpoint_ids: None,
        });
        debug_adapter.send_event("stopped", event_body)?;
        tracing::trace!("Notified DAP client that the core halted: {:?}", status);
        Ok(())
    }

    /// Update `last_known_status` and emit the appropriate DAP event for a
    /// status transition, without a live `Core`. Semihosting halts are
    /// skipped here (the poll loop handles them separately) and the PC for
    /// the `stopped` event is read via [`Self::notify_halted`].
    async fn process_core_status(
        &mut self,
        debug_adapter: &mut DebugAdapter,
        cd_idx: usize,
        status: CoreStatus,
    ) -> Result<CoreStatus, DebuggerError> {
        if status == self.core_data[cd_idx].last_known_status {
            return Ok(status);
        }
        self.core_data[cd_idx].last_known_status = status;

        match status {
            CoreStatus::Running | CoreStatus::Sleeping => {
                let event_body = Some(ContinuedEventBody {
                    all_threads_continued: Some(true),
                    thread_id: cd_idx as i64,
                });
                debug_adapter.send_event("continued", event_body)?;
                tracing::trace!("Notified DAP client that the core continued: {:?}", status);
            }
            CoreStatus::Halted(HaltReason::Step) => {}
            CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(_))) => {}
            CoreStatus::Halted(_) => self.notify_halted(debug_adapter, cd_idx, status).await?,
            CoreStatus::LockedUp => {
                debug_adapter.show_message(
                    MessageSeverity::Warning,
                    format!("Core {} is in locked up state", cd_idx),
                );
                self.notify_halted(debug_adapter, cd_idx, status).await?;
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

    /// Attach to the target's server-owned RTT interface over RPC.
    async fn attach_to_rtt(
        &mut self,
        debug_adapter: &mut DebugAdapter,
        cd_idx: usize,
        program_binary: Option<&Path>,
        rtt_config: &RttConfig,
        timestamp_offset: UtcOffset,
    ) -> Result<()> {
        if self.core_data[cd_idx].rtt_connection.is_some() {
            return Ok(());
        }

        let mut defmt_data = self.core_data[cd_idx].defmt_state.clone();
        let use_auto_formats = rtt_config.channels.is_empty();

        let mut build_up_channel = |debug_adapter: &mut DebugAdapter,
                                    number: u32,
                                    channel_name: &str|
         -> Result<debug_rtt::DebuggerRttChannel> {
            let mut channel_config = rtt_config.channel_config(number).clone();

            if use_auto_formats {
                channel_config.data_format = if channel_name == "defmt" {
                    DataFormat::Defmt
                } else {
                    DataFormat::String
                };
            }

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

            debug_adapter.rtt_window(number, channel_name.to_string(), data_format);

            Ok(debug_rtt::DebuggerRttChannel {
                channel_number: number,
                has_client_window: false,
                channel_data_format,
            })
        };

        let (client, up_channels, down_channels): (
            debug_rtt::RemoteRttClient,
            ChannelNames,
            ChannelNames,
        ) = {
            let rtt_key = self.ensure_rtt_client(cd_idx, rtt_config).await?;
            let session = self.backend.session_interface();

            let channels = session
                .get_rtt_channels(rtt_key)
                .await
                .map_err(|e| anyhow!("Failed to query remote RTT channels: {e}"))?;

            if channels.up.is_empty() && channels.down.is_empty() {
                return Ok(());
            }

            let up = channels
                .up
                .into_iter()
                .map(|m| (m.number, m.name))
                .collect();
            let down = channels
                .down
                .into_iter()
                .map(|m| (m.number, m.name))
                .collect();

            let client = debug_rtt::RemoteRttClient::new(session, rtt_key);

            (client, up, down)
        };

        let mut debugger_rtt_channels = vec![];
        for (number, name) in &up_channels {
            debugger_rtt_channels.push(build_up_channel(debug_adapter, *number, name)?);
        }
        self.core_data[cd_idx].defmt_state = defmt_data;

        for (number, name) in &down_channels {
            debug_adapter.open_prompt(PromptKind::Rtt, name, *number);
        }

        self.core_data[cd_idx].rtt_connection = Some(debug_rtt::RttConnection {
            client,
            debugger_rtt_channels,
        });

        Ok(())
    }

    /// The target has no way of notifying the debug adapter when things change,
    /// so the DAP server periodically queries core status through RPC to determine:
    /// - Whether the target cores are running, and what their actual status is.
    /// - Whether the target cores have data in their RTT buffers that we need to read and pass to the client.
    ///
    /// To optimize this polling process while also optimizing the reading of RTT data, we apply a couple of principles:
    /// 1. Sleep (nap for a short duration) between polling each target core, but:
    /// - Only sleep IF the core's status hasn't changed AND there was no RTT data in the last poll.
    /// - Otherwise move on without delay, to keep things flowing as fast as possible.
    /// - The justification is that any client side CPU used to keep polling is a small price to pay for maximum throughput of debug requests and RTT from the probe.
    /// 2. Check all target cores to ensure they have a configured and initialized RTT connections and if they do, process the RTT data.
    /// - To keep things efficient, the polling of RTT data is done only when we expect there to be data available.
    /// - We check for RTT only when the core has an RTT connection configured, and one of the following is true:
    ///   - While the core is NOT halted, because core processing can generate new data at any time.
    ///   - The first time we have entered halted status, to ensure the buffers are drained. After that, for as long as we remain in halted state, we don't need to check RTT again.
    ///
    /// Return a boolean indicating whether we should consider a short delay before the next poll.
    #[tracing::instrument(level = "trace", skip_all)]
    pub(crate) async fn poll_cores(
        &mut self,
        session_config: &SessionConfig,
        debug_adapter: &mut DebugAdapter,
    ) -> Result<bool, DebuggerError> {
        // By default, we will have a small delay between polls, and will disable it if
        // we know the last poll returned data, on the assumption that there might be at least one more batch of data.
        let mut suggest_delay_required = true;

        let timestamp_offset = self.timestamp_offset;

        let cores_halted_previously = debug_adapter.all_cores_halted;

        // Always set `all_cores_halted` to true, until one core is found to be running.
        debug_adapter.all_cores_halted = true;

        // Cores that transitioned to halted during this poll and need their
        // stack display cache rebuilt. Collect them here and perform the
        // server-owned unwinds after status, RTT, and semihosting processing.
        let mut needs_unwind: Vec<usize> = Vec::new();

        for core_config in session_config.core_configs.iter() {
            // Fetch status via the backend before `attach_core` so the
            // `&mut self.backend` borrow is released before `attach_core`
            // reborrows it.
            let core_index = core_config.core_index;

            let Some(cd_idx) = self
                .core_data
                .iter()
                .position(|c| c.core_index == core_index)
            else {
                tracing::debug!("No core data for core #{core_index}; cannot poll.");
                continue;
            };

            let current_core_status = if debug_adapter.configuration_is_done() {
                match self.backend.status(core_index).await {
                    Ok(status) => status,
                    Err(error) => {
                        let err = DebuggerError::from(error);
                        let _ = debug_adapter.show_error_message(&err);

                        let cd = &mut self.core_data[cd_idx];
                        cd.last_known_status = CoreStatus::Unknown;
                        cd.invalidate_stack_frame_cache();

                        return Err(err);
                    }
                }
            } else {
                CoreStatus::Unknown
            };

            let previous_core_status = self.core_data[cd_idx].last_known_status;

            let mut current_core_status = self
                .process_core_status(debug_adapter, cd_idx, current_core_status)
                .await
                .inspect_err(|error| {
                    let _ = debug_adapter.show_error_message(error);
                })?;

            let semihosting_command = match current_core_status {
                CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(c))) => {
                    Some(c)
                }
                _ => None,
            };

            let rtt_enabled = core_config.rtt_config.enabled;

            // RTT attach and polling drive the server-owned `RttClient`
            // through RPC. The DAP side retains only the RPC handle and
            // channel/display metadata.
            if rtt_enabled {
                if let Some(core_rtt) = self.core_data[cd_idx].rtt_connection.as_mut() {
                    let had_data = core_rtt.process_rtt_data_remote(debug_adapter).await;
                    if had_data {
                        suggest_delay_required = false;
                    }
                } else if debug_adapter.configuration_is_done()
                    && let Err(error) = self
                        .attach_to_rtt(
                            debug_adapter,
                            cd_idx,
                            core_config.program_binary.as_deref(),
                            &core_config.rtt_config,
                            timestamp_offset,
                        )
                        .await
                {
                    debug_adapter
                        .show_error_message(&DebuggerError::Other(error))
                        .ok();
                }
            }

            // Semihosting handling runs via `RpcBackend::handle_semihosting`;
            // the RPC server owns the live core and semihosting state. The
            // backend returns UI events (RTT window open, console/RTT output)
            // that we replay on the DAP adapter here.
            //
            // An unhandled command (for example `GetCommandLine` from
            // embedded-test) leaves the core halted. Call the handler only
            // on a new halt, otherwise the poll loop warns forever.
            if semihosting_command.is_some() && previous_core_status != current_core_status {
                let result = self.backend.handle_semihosting(core_index).await?;
                for event in result.events {
                    match event {
                        crate::cmd::dap_server::backend::SemihostingUiEvent::RttWindow {
                            handle,
                            path,
                            format,
                        } => {
                            debug_adapter.rtt_window(handle, path, format);
                        }
                        crate::cmd::dap_server::backend::SemihostingUiEvent::LogToConsole(msg) => {
                            debug_adapter.log_to_console(msg);
                        }
                        crate::cmd::dap_server::backend::SemihostingUiEvent::RttOutput {
                            handle,
                            data,
                        } => {
                            debug_adapter.rtt_output(handle, data);
                        }
                    }
                }
                current_core_status = result.status;

                if current_core_status.is_halted() {
                    // The earlier status query did not observe the
                    // post-semihosting halt, so notify the client now.
                    self.notify_halted(debug_adapter, cd_idx, current_core_status)
                        .await?;
                } else {
                    // If the semihosting command was handled, we do not need to suggest a delay.
                    suggest_delay_required = false;
                }
                self.core_data[cd_idx].last_known_status = current_core_status;
            }

            // Non-semihosting status change: log the new status (PC read via
            // the backend).
            if current_core_status != previous_core_status && semihosting_command.is_none() {
                let pc = if current_core_status.is_halted() {
                    self.backend.program_counter(core_index).await
                } else {
                    None
                };
                debug_adapter.log_to_console(current_core_status.short_long_status(pc).1);
            }

            // If the core is running, we set the flag to indicate that at least one core is not halted.
            // By setting it here, we ensure that RTT will be checked at least once after the core has halted.
            if !current_core_status.is_halted() {
                // Stack metadata is only a display cache for a halted target.
                // Once execution resumes (or status is uncertain), retaining
                // it could expose frame ids that no longer exist server-side.
                self.core_data[cd_idx].invalidate_stack_frame_cache();
                debug_adapter.all_cores_halted = false;
            } else if !cores_halted_previously {
                // If currently halted, and was previously running
                // update the stack frames
                let _stackframe_span = tracing::debug_span!("Update Stack Frames").entered();
                tracing::debug!("Updating the stack frame data for core #{}", core_index);

                // Keep the display cache empty until the complete RPC unwind
                // succeeds. An unwind error must never leave the previous
                // halt's frames visible.
                self.core_data[cd_idx].invalidate_stack_frame_cache();

                // RPC stack state and DebugInfo are owned by the server.
                needs_unwind.push(core_index);
            }
        }

        // Ask the RPC server to unwind each newly halted core, then replace
        // the client metadata-only display cache with the returned frames.
        for &core_index in &needs_unwind {
            let frames = self
                .backend
                .unwind_stack(core_index, 500)
                .await
                .map_err(DebuggerError::ProbeRs)?;

            if let Some(core_data) = self
                .core_data
                .iter_mut()
                .find(|cd| cd.core_index == core_index)
            {
                core_data.replace_stack_frame_cache(frames);
            }
        }
        Ok(suggest_delay_required)
    }

    pub(crate) async fn clean_up(
        &mut self,
        session_config: &SessionConfig,
    ) -> Result<(), DebuggerError> {
        for core_config in session_config.core_configs.iter() {
            if core_config.rtt_config.enabled {
                let Some(cd_idx) = self
                    .core_data
                    .iter()
                    .position(|c| c.core_index == core_config.core_index)
                else {
                    continue;
                };
                if let Some(core_rtt) = self.core_data[cd_idx].rtt_connection.as_mut() {
                    core_rtt.clean_up_async().await?;
                }
            }
        }

        Ok(())
    }
}

/// Apply the session config's requested working directory if one was
/// supplied. Shared between the local and RPC attach paths.
///
/// Skipped in `remote_server_mode`: there `cwd` is a path on the *client's* filesystem
/// (kept around as a display-only string for log messages), so calling `set_current_dir`
/// with it on the server would fail. Relative-path resolution against `cwd` does not
/// apply in remote mode — every client-supplied file path arrives either already absolute,
/// or materialized by [`SessionConfig::materialize_uploaded_files`] to an absolute temp
/// path — so the working directory does not need to change for downstream code to work.
fn apply_session_cwd(config: &configuration::SessionConfig) -> Result<(), DebuggerError> {
    if !config.remote_server_mode
        && let Some(new_cwd) = config.cwd.clone()
    {
        set_current_dir(new_cwd.as_path()).map_err(|err| {
            anyhow!("Failed to set current working directory to: {new_cwd:?}, {err:?}")
        })?;
    }
    Ok(())
}

/// Build the per-core [`CoreData`] record the DAP server uses to track a
/// debug session. Called once per configured core by [`initialize_core_data`].
fn build_core_data(
    core_configuration: &CoreConfig,
    target_name: &str,
) -> Result<CoreData, DebuggerError> {
    let mut repl_commands = REPL_COMMANDS.to_vec();
    let mut test_data: Box<dyn Any> = Box::new(());
    if let Some(path_to_elf) = core_configuration.program_binary.as_deref()
        && let Some(elf_info) = EmbeddedTestElfInfo::from_elf(path_to_elf)?
    {
        tracing::debug!("Embedded Test Metadata: {:?}", elf_info);
        if elf_info.version != 1 {
            tracing::info!("Detected unsupported embedded-test version in ELF file.");
        } else {
            tracing::info!(
                "Detected embedded-test in ELF file. Adding `test` command to Debug Console."
            );

            repl_commands.push(EMBEDDED_TEST);
            test_data = Box::new(elf_info);
        }
    }

    Ok(CoreData {
        core_index: core_configuration.core_index,
        last_known_status: CoreStatus::Unknown,
        target_name: format!("{}-{}", core_configuration.core_index, target_name),
        stack_frames: vec![],
        breakpoints: vec![],
        rtt_scan_ranges: WireScanRegion::Ranges(vec![]),
        rtt_connection: None,
        defmt_state: None,
        rtt_remote_handle: None,
        repl_commands,
        test_data,
    })
}

/// Run the shared "post-attach" core initialization: enforce the
/// single-core invariant, filter config entries down to cores that
/// actually exist on the target, apply vector-catch settings, and build
/// the [`CoreData`] vector.
fn initialize_core_data(
    backend: &mut RpcBackend,
    config: &configuration::SessionConfig,
) -> Result<Vec<CoreData>, DebuggerError> {
    if config.core_configs.len() != 1 {
        // TODO: For multi-core, allow > 1.
        return Err(DebuggerError::Other(anyhow!(
            "probe-rs-debugger requires that one, and only one, core be configured for debugging."
        )));
    }

    let available_cores = backend.target_metadata.cores.clone();
    let target_name = backend.target_metadata.target_name.clone();

    let valid_core_configs = config.core_configs.iter().filter(|&core_config| {
        available_cores
            .iter()
            .any(|(target_core_index, _)| *target_core_index == core_config.core_index)
    });

    let mut core_data_vec = vec![];
    for core_configuration in valid_core_configs {
        let core_data = build_core_data(core_configuration, &target_name)?;
        core_data_vec.push(core_data);
    }
    Ok(core_data_vec)
}
