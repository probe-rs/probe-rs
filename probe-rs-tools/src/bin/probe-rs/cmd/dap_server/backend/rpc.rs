//! RPC backend for the DAP server.
//!
//! [`RpcBackend`] proxies all session/core operations to a probe-rs RPC
//! server through [`probe_rs_rpc_client::RpcClient`]. The DAP session loop is
//! driven from a [`tokio::task::spawn_blocking`] task on a multi-threaded
//! runtime, so async round trips can be `.await`ed directly — no
//! `block_on`/`block_in_place` bridge is required.

use std::{collections::HashMap, path::PathBuf, time::Duration};

use super::{SemihostingHandleResult, SemihostingUiEvent};
use crate::cmd::dap_server::DebuggerError;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::{
    DisassembledInstruction, EvaluateArguments, EvaluateResponseBody, Scope, Source, Variable,
};
use crate::cmd::dap_server::server::configuration::FlashingConfig;
use crate::rpc::{
    Key, RttClient, Session,
    functions::{
        breakpoints::convert::from_wire_source_location,
        core_ops::convert::{
            from_wire_core_information, from_wire_core_status, from_wire_core_type,
            from_wire_instruction_set, from_wire_register_id, from_wire_register_value,
            to_wire_register_id, to_wire_register_value, to_wire_vector_catch_condition,
        },
    },
};
use probe_rs::{
    Architecture, CoreInformation, CoreRegisters, CoreStatus, CoreType, Error, RegisterId,
    RegisterValue, VectorCatchCondition,
};
use probe_rs_debug::{
    ColumnType, DebugRegisters, ObjectRef, SourceLocation as DebugSourceLocation, StackFrame,
    SteppingMode, TypedPath, VerifiedBreakpoint,
};
use probe_rs_rpc::RpcError;
use probe_rs_rpc::breakpoints::{
    SourceBreakpointLocation, WireSourceLocation as WireBreakpointSourceLocation,
};
use probe_rs_rpc::core_ops::{WireCoreMetadata, WireCoreStatus, WireRegisterId, WireSteppingMode};
use probe_rs_rpc::disassemble::{WireDisassembledInstruction, WireSource};
use probe_rs_rpc::flash::{
    DownloadOptions as WireDownloadOptions, ProgressEvent as WireProgressEvent, VerifyResult,
};
use probe_rs_rpc::info::WireSessionTargetMetadata;
use probe_rs_rpc::stack_trace::{
    RichStackTraces, SourceLocation as WireSourceLocation, WireDebugRegister,
};
use probe_rs_rpc_client::{
    CoreInterface as RpcCoreClient, ResolvedUpload, RpcClient, SessionInterface,
};

/// Convert an [`anyhow::Error`] coming out of the RPC client into the
/// [`probe_rs::Error`] surface the DAP server expects.
pub(crate) fn rpc_err(err: impl Into<anyhow::Error>) -> Error {
    let err = err.into();
    // Typed `probe_rs::Error` values lose their variant when crossing the
    // RPC boundary (they come back as an opaque `anyhow::Error` carrying only
    // the `Display` text). Best-effort reconstruct the common variants call
    // sites care about, so features such as vector catch silently fall back
    // on targets that don't support them instead of spamming ERROR logs.
    let text = format!("{err:#}");
    if text.contains("has not yet been implemented") {
        return Error::NotImplemented("remote operation");
    }
    Error::Other(format!("{err:?}"))
}

/// Rebuild a [`DebugRegisters`] from a wire register dump, using the same
/// static [`CoreRegisters`] file the server used so the resulting
/// `DebugRegister` ordering and DWARF ids match what the server's `unwind`
/// started from. The result is metadata for the client stack display cache;
/// all DWARF interpretation remains server-side.
pub(crate) fn rebuild_debug_registers(
    regs: &'static CoreRegisters,
    wire: &[WireDebugRegister],
) -> DebugRegisters {
    let mut lookup: HashMap<RegisterId, RegisterValue> = HashMap::with_capacity(wire.len());
    for w in wire {
        if let Some(value) = w.value {
            lookup.insert(from_wire_register_id(w.id), from_wire_register_value(value));
        }
    }
    DebugRegisters::from_core_registers(regs, |rid| lookup.get(rid).copied())
}

/// Convert a wire `probe_rs_rpc::stack_trace::SourceLocation` (resolved server-side) back into a
/// `probe_rs_debug::SourceLocation`. The wire form encodes `LeftEdge`
/// columns as `1`, so they round-trip as `Column(1)` — a minor, UI-irrelevant
/// difference for the RPC path.
pub(crate) fn from_wire_location(w: &WireSourceLocation) -> DebugSourceLocation {
    DebugSourceLocation {
        path: TypedPath::derive(w.file.as_bytes()).to_path_buf(),
        line: w.line,
        column: w.column.map(ColumnType::Column),
        address: None,
    }
}

/// Small session-scoped target facts returned by the RPC server so the DAP
/// client does not need a local [`probe_rs::Target`] mirror.
#[derive(Clone)]
pub struct SessionTargetMetadata {
    pub target_name: String,
    pub default_format: Option<String>,
    pub cores: Vec<(usize, CoreType)>,
}

/// A DAP backend that drives a remote target over RPC.
pub struct RpcBackend {
    client: RpcClient,
    sessid: Key<Session>,
    pub(crate) target_metadata: SessionTargetMetadata,
    /// Per-core metadata cached at attach-time so static-property methods
    /// (register file, architecture, ...) need no round trip.
    pub(crate) core_metadata: Vec<CoreMetadata>,
}

#[derive(Clone)]
pub(crate) struct CoreMetadata {
    pub(crate) architecture: Architecture,
    pub(crate) registers: &'static CoreRegisters,
}

/// Per-core information the [`RpcBackend`] caller has to gather at attach
/// time. Most of these are static properties of the core type once the
/// target has booted, so a single query is enough.
#[derive(Clone, Copy)]
pub struct CorePerAttachInfo {
    pub architecture: Architecture,
    pub fpu_support: bool,
    pub fp_register_count: Option<usize>,
}

impl CorePerAttachInfo {
    pub(crate) async fn query(
        client: &RpcClient,
        sessid: Key<Session>,
        core_index: usize,
        architecture: Architecture,
    ) -> Result<Self, Error> {
        let metadata = RpcCoreClient::new_for_backend(client.clone(), sessid, core_index as u32)
            .metadata()
            .await
            .map_err(rpc_err)?;
        Ok(Self::from_wire(architecture, metadata))
    }

    /// Placeholder for a core the debugger will not attach to.
    ///
    /// A core that firmware has not enabled yet cannot be queried, and the
    /// register file selected here only affects the core being debugged.
    pub(crate) fn not_debugged(architecture: Architecture) -> Self {
        Self {
            architecture,
            fpu_support: false,
            fp_register_count: None,
        }
    }

    fn from_wire(architecture: Architecture, metadata: WireCoreMetadata) -> Self {
        Self {
            architecture,
            fpu_support: metadata.fpu_support,
            fp_register_count: metadata
                .floating_point_register_count
                .map(|count| count as usize),
        }
    }
}

impl RpcBackend {
    /// The RPC client backing this session, used for session-level
    /// operations that are not expressed as core round trips
    pub(crate) fn session_interface(&self) -> SessionInterface {
        SessionInterface::new(self.client.clone(), self.sessid)
    }

    fn core(&self, core_index: usize) -> RpcCoreClient {
        RpcCoreClient::new_for_backend(self.client.clone(), self.sessid, core_index as u32)
    }

    /// Build a new RPC backend.
    ///
    /// The caller is responsible for:
    /// * having already completed `probe/attach` over RPC (yielding a
    ///   `Key<Session>`),
    /// * fetching [`SessionTargetMetadata`] from the server (`target/metadata`)
    ///   so core lists and image-format defaults match the attached session,
    /// * supplying per-core metadata: either by querying the server at
    ///   attach-time or by inferring it from the target description.
    pub fn new(
        client: RpcClient,
        sessid: Key<Session>,
        target_metadata: SessionTargetMetadata,
        per_core: Vec<CorePerAttachInfo>,
    ) -> Self {
        let cores = target_metadata.cores.clone();
        let core_metadata = cores
            .iter()
            .zip(per_core)
            .map(|((_, core_type), info)| {
                let registers = CoreRegisters::for_core_type(
                    *core_type,
                    info.fpu_support,
                    info.fp_register_count,
                );
                CoreMetadata {
                    architecture: info.architecture,
                    registers,
                }
            })
            .collect();

        Self {
            client,
            sessid,
            target_metadata,
            core_metadata,
        }
    }

    pub(crate) async fn read_memory_8(
        &self,
        core_index: usize,
        address: u64,
        count: usize,
    ) -> Result<Vec<u8>, Error> {
        self.core(core_index)
            .read_memory_8(address, count)
            .await
            .map_err(rpc_err)
    }

    pub(crate) async fn read_bytes(
        &self,
        core_index: usize,
        address: u64,
        count: usize,
    ) -> Result<Vec<u8>, Error> {
        self.core(core_index)
            .read_bytes(address, count)
            .await
            .map_err(rpc_err)
    }

    pub(crate) async fn write_memory_8(
        &self,
        core_index: usize,
        address: u64,
        data: Vec<u8>,
    ) -> Result<(), Error> {
        self.core(core_index)
            .write_memory_8(address, data)
            .await
            .map_err(rpc_err)
    }

    pub(crate) async fn reset_and_halt(
        &self,
        core_index: usize,
        timeout: Duration,
    ) -> Result<CoreInformation, Error> {
        self.core(core_index)
            .reset_and_halt(timeout)
            .await
            .map(from_wire_core_information)
            .map_err(rpc_err)
    }

    pub(crate) async fn kickoff_test(&self, core_index: usize, address: u64) -> Result<(), Error> {
        self.core(core_index)
            .kickoff_test(address)
            .await
            .map_err(rpc_err)
    }

    pub(crate) async fn resolve_source_breakpoints(
        &self,
        locations: Vec<SourceBreakpointLocation>,
    ) -> Result<Vec<Result<VerifiedBreakpoint, String>>, Error> {
        let resolved = self
            .session_interface()
            .resolve_source_breakpoints(locations)
            .await
            .map_err(rpc_err)?;
        Ok(resolved
            .into_iter()
            .map(
                |resolution| match (resolution.breakpoint, resolution.error) {
                    (Some(breakpoint), _) => Ok(VerifiedBreakpoint {
                        address: breakpoint.address,
                        source_location: from_wire_source_location(breakpoint.source_location),
                    }),
                    (None, Some(error)) => Err(error),
                    (None, None) => {
                        Err("Server returned an empty breakpoint resolution.".to_string())
                    }
                },
            )
            .collect())
    }

    pub(crate) async fn resolve_source_locations(
        &self,
        addresses: Vec<u64>,
    ) -> Result<Vec<Option<DebugSourceLocation>>, Error> {
        self.session_interface()
            .resolve_source_locations(addresses)
            .await
            .map(|locations| {
                locations
                    .into_iter()
                    .map(|location: Option<WireBreakpointSourceLocation>| {
                        location.map(from_wire_source_location)
                    })
                    .collect()
            })
            .map_err(rpc_err)
    }

    /// The wire conversion surfaces `GetCommandLine` as a placeholder
    /// `SemihostingCommand`; the server-side `core/handle_semihosting`
    /// endpoint re-derives the real command from the live core, so the
    /// client never needs target memory access here.
    pub(crate) async fn status(&mut self, core_index: usize) -> Result<CoreStatus, Error> {
        let client = self.core(core_index);
        let wire: WireCoreStatus = client.status().await.map_err(rpc_err)?;
        Ok(from_wire_core_status(wire))
    }

    /// Set a batch of hardware breakpoints, reporting per-address failures in
    /// place rather than failing the whole batch.
    pub(crate) async fn set_hw_breakpoints(
        &mut self,
        core_index: usize,
        addresses: Vec<u64>,
    ) -> Result<Vec<Result<(), RpcError>>, Error> {
        let client = self.core(core_index);
        client.set_hw_breakpoints(addresses).await.map_err(rpc_err)
    }

    pub(crate) async fn clear_hw_breakpoints(
        &mut self,
        core_index: usize,
        addresses: Vec<u64>,
    ) -> Result<(), Error> {
        let client = self.core(core_index);
        client
            .clear_hw_breakpoints(addresses)
            .await
            .map_err(rpc_err)
    }

    pub(crate) async fn halt(
        &mut self,
        core_index: usize,
        timeout: Duration,
    ) -> Result<CoreInformation, Error> {
        let client = self.core(core_index);
        let info = client.halt(timeout).await.map_err(rpc_err)?;
        Ok(from_wire_core_information(info))
    }

    pub(crate) async fn run(&mut self, core_index: usize) -> Result<(), Error> {
        // The server-owned VariableCache is about to go stale: drop it so
        // stale variable handles are not served before the next halt
        // rebuilds them.
        self.session_interface()
            .clear_core_debug_state(core_index as u32)
            .await
            .map_err(rpc_err)?;
        let client = self.core(core_index);
        client.run().await.map_err(rpc_err)
    }

    pub(crate) async fn core_halted(&mut self, core_index: usize) -> Result<bool, Error> {
        Ok(self.status(core_index).await?.is_halted())
    }

    pub(crate) async fn program_counter_id(
        &mut self,
        core_index: usize,
    ) -> Result<RegisterId, Error> {
        self.core_metadata
            .get(core_index)
            .and_then(|m| m.registers.pc())
            .map(|r| r.id)
            .ok_or_else(|| Error::Other(format!("No PC register for core {core_index}")))
    }

    pub(crate) async fn set_hw_breakpoint(
        &mut self,
        core_index: usize,
        address: u64,
    ) -> Result<(), Error> {
        let client = self.core(core_index);
        client.set_hw_breakpoint(address).await.map_err(rpc_err)
    }

    pub(crate) async fn set_variable(
        &mut self,
        core_index: usize,
        parent_key: i64,
        name: String,
        value: String,
    ) -> Result<probe_rs_rpc::debug_vars::WireSetVariableResponse, Error> {
        self.session_interface()
            .set_variable(core_index as u32, parent_key, name, value)
            .await
            .map_err(rpc_err)
    }

    pub(crate) async fn disassemble(
        &mut self,
        core_index: usize,
        memory_reference: u64,
        byte_offset: i64,
        instruction_offset: i64,
        instruction_count: i64,
    ) -> Result<
        Vec<crate::cmd::dap_server::debug_adapter::dap::dap_types::DisassembledInstruction>,
        Error,
    > {
        let wire = self
            .session_interface()
            .disassemble(
                core_index as u32,
                memory_reference,
                byte_offset,
                instruction_offset,
                instruction_count,
            )
            .await
            .map_err(rpc_err)?;
        Ok(wire.into_iter().map(Into::into).collect())
    }

    pub(crate) async fn read_core_reg(
        &mut self,
        core_index: usize,
        register_id: RegisterId,
    ) -> Result<RegisterValue, Error> {
        let client = self.core(core_index);
        let wire = client
            .read_core_reg(to_wire_register_id(register_id))
            .await
            .map_err(rpc_err)?;
        Ok(from_wire_register_value(wire))
    }

    pub(crate) async fn write_core_reg(
        &mut self,
        core_index: usize,
        register_id: RegisterId,
        value: RegisterValue,
    ) -> Result<(), Error> {
        let client = self.core(core_index);
        client
            .write_core_reg(
                to_wire_register_id(register_id),
                to_wire_register_value(value),
            )
            .await
            .map_err(rpc_err)
    }

    pub(crate) async fn read_core_registers(
        &mut self,
        core_index: usize,
        ids: Vec<RegisterId>,
    ) -> Result<Vec<Option<RegisterValue>>, Error> {
        let wire_ids: Vec<WireRegisterId> = ids.iter().copied().map(to_wire_register_id).collect();
        let client = self.core(core_index);
        let results = client.read_registers(wire_ids).await.map_err(rpc_err)?;
        Ok(results
            .into_iter()
            .map(|r| r.result.ok().map(from_wire_register_value))
            .collect())
    }

    pub(crate) async fn dump_core(
        &mut self,
        core_index: usize,
        ranges: Vec<std::ops::Range<u64>>,
    ) -> Result<probe_rs::CoreDump, Error> {
        let client = self.core(core_index);
        let wire = client.dump_core(ranges).await.map_err(rpc_err)?;
        Ok(probe_rs::CoreDump {
            registers: wire
                .registers
                .into_iter()
                .map(|(id, v)| (from_wire_register_id(id), from_wire_register_value(v)))
                .collect(),
            data: wire.data,
            instruction_set: from_wire_instruction_set(wire.instruction_set),
            supports_native_64bit_access: wire.supports_native_64bit_access,
            core_type: from_wire_core_type(wire.core_type),
            fpu_support: wire.fpu_support,
            floating_point_register_count: wire.floating_point_register_count.map(|c| c as usize),
        })
    }

    pub(crate) async fn handle_semihosting(
        &mut self,
        core_index: usize,
    ) -> Result<SemihostingHandleResult, Error> {
        use probe_rs_rpc::core_ops::WireSemihostingUiEvent;

        let client = self.core(core_index);
        let result = client.handle_semihosting().await.map_err(rpc_err)?;
        let events = result
            .events
            .into_iter()
            .map(|e| match e {
                WireSemihostingUiEvent::RttWindow {
                    handle,
                    path,
                    format,
                } => SemihostingUiEvent::RttWindow {
                    handle,
                    path,
                    format,
                },
                WireSemihostingUiEvent::LogToConsole(s) => SemihostingUiEvent::LogToConsole(s),
                WireSemihostingUiEvent::RttOutput { handle, data } => {
                    SemihostingUiEvent::RttOutput { handle, data }
                }
            })
            .collect();
        Ok(SemihostingHandleResult {
            status: from_wire_core_status(result.status),
            events,
        })
    }

    pub(crate) async fn enable_vector_catch(
        &mut self,
        core_index: usize,
        condition: VectorCatchCondition,
    ) -> Result<(), Error> {
        let client = self.core(core_index);
        client
            .enable_vector_catch(to_wire_vector_catch_condition(condition))
            .await
            .map_err(rpc_err)
    }

    /// Halt if running, enable each requested condition, then resume if the
    /// core was halted on entry.
    pub(crate) async fn apply_vector_catch(
        &mut self,
        core_index: usize,
        config: &crate::cmd::dap_server::server::configuration::CoreConfig,
    ) -> Result<(), Error> {
        let needs_vector_catch =
            config.catch_hardfault || config.catch_reset || config.catch_svc || config.catch_hlt;
        if !needs_vector_catch {
            return Ok(());
        }
        let was_halted = self.core_halted(core_index).await?;
        if !was_halted {
            self.halt(core_index, Duration::from_millis(100)).await?;
        }
        let requested: [(bool, VectorCatchCondition); 4] = [
            (config.catch_hardfault, VectorCatchCondition::HardFault),
            (config.catch_reset, VectorCatchCondition::CoreReset),
            (config.catch_svc, VectorCatchCondition::Svc),
            (config.catch_hlt, VectorCatchCondition::Hlt),
        ];
        for (enabled, condition) in requested {
            if enabled && let Err(e) = self.enable_vector_catch(core_index, condition).await {
                // A target that has no vector catch must not raise an error
                // popup in the client on every attach.
                if matches!(e, Error::NotImplemented(_)) {
                    tracing::debug!("The target does not support vector catch: {e}");
                } else {
                    tracing::error!("Failed to enable_vector_catch: {e}");
                }
            }
        }
        if was_halted {
            self.run(core_index).await?;
        }
        Ok(())
    }

    pub(crate) async fn debug_step(
        &mut self,
        core_index: usize,
        mode: SteppingMode,
    ) -> Result<(CoreStatus, u64, Option<String>), Error> {
        let wire_mode = match mode {
            SteppingMode::StepInstruction => WireSteppingMode::StepInstruction,
            SteppingMode::OverStatement => WireSteppingMode::OverStatement,
            SteppingMode::IntoStatement => WireSteppingMode::IntoStatement,
            SteppingMode::OutOfStatement => WireSteppingMode::OutOfStatement,
            // Not exposed over the wire; map to a safe default.
            SteppingMode::BreakPoint => WireSteppingMode::OverStatement,
        };
        let resp = self
            .session_interface()
            .debug_step(core_index as u32, wire_mode)
            .await
            .map_err(rpc_err)?;
        Ok((
            from_wire_core_status(resp.status),
            resp.program_counter,
            resp.warning,
        ))
    }

    /// Single-round-trip stack unwind: the server unwinds AND owns the
    /// `local_variables`/`static_variables` caches (keyed by `sessid`+core),
    /// returning per-frame metadata plus the server-assigned frame id. We
    /// rebuild lightweight [`StackFrame`]s with `local_variables: None` —
    /// subsequent `scopes`/`variables` requests resolve server-side. Source
    /// locations are taken from the wire (the server already resolved them).
    pub(crate) async fn unwind_stack(
        &mut self,
        core_index: usize,
        max_frames: usize,
    ) -> Result<Vec<StackFrame>, Error> {
        let session = self.session_interface();
        let rich: RichStackTraces = session
            .take_rich_stack_trace(Some(core_index as u32), max_frames as u32)
            .await
            .map_err(rpc_err)?;

        // The server omits cores it cannot unwind: one that is not enabled, or
        // any core in a session that has no debug info. Neither is an error
        // here — the core simply has no frames to display.
        let Some(rich_core) = rich.cores.into_iter().find(|c| c.core == core_index as u32) else {
            return Ok(Vec::new());
        };

        // Clone metadata before borrowing `self` via `self.core(...)`.
        let metadata = self
            .target_metadata
            .cores
            .iter()
            .zip(self.core_metadata.iter())
            .find_map(|((idx, _), meta)| (*idx == core_index).then_some(meta.clone()))
            .ok_or(Error::CoreNotFound(core_index))?;

        let wire_frames = rich_core.frames;
        let mut frames: Vec<StackFrame> = Vec::with_capacity(wire_frames.len());

        let mut idx = 0;
        while idx < wire_frames.len() {
            let group_start = idx;
            let group_regs = wire_frames[idx].registers.clone();
            while idx < wire_frames.len() && wire_frames[idx].registers == group_regs {
                idx += 1;
            }
            let group = &wire_frames[group_start..idx];
            let cfa = group[0].canonical_frame_address;
            let registers = rebuild_debug_registers(metadata.registers, &group_regs);

            for wf in group {
                let pc_value: RegisterValue = from_wire_register_value(wf.program_counter);
                frames.push(StackFrame {
                    id: ObjectRef::from(wf.id as i64),
                    function_name: wf.function_name.clone(),
                    source_location: wf.location.as_ref().map(from_wire_location),
                    registers: registers.clone(),
                    pc: pc_value,
                    frame_base: wf.frame_base,
                    is_inlined: wf.is_inlined,
                    local_variables: None,
                    canonical_frame_address: cfa,
                });
            }
        }

        Ok(frames)
    }

    /// Synchronize the server's per-core SVD state with the configured path.
    /// `None` removes any existing peripheral cache. Non-fatal: callers
    /// should warn and continue on error.
    pub(crate) async fn load_svd(
        &mut self,
        core_index: usize,
        path: Option<PathBuf>,
    ) -> Result<(), Error> {
        self.session_interface()
            .load_svd(core_index as u32, path)
            .await
            .map_err(rpc_err)?;
        Ok(())
    }

    pub(crate) async fn program_counter(&mut self, core_index: usize) -> Option<u64> {
        match self.program_counter_id(core_index).await {
            Ok(id) => self
                .read_core_reg(core_index, id)
                .await
                .ok()
                .and_then(|v| v.try_into().ok()),
            Err(_) => None,
        }
    }

    pub(crate) async fn scopes(
        &mut self,
        core_index: usize,
        frame_id: u32,
    ) -> Result<Vec<Scope>, Error> {
        let session = self.session_interface();
        let wire = session
            .scopes(core_index as u32, frame_id)
            .await
            .map_err(rpc_err)?;
        Ok(wire
            .into_iter()
            .map(|w| Scope {
                name: w.name,
                presentation_hint: w.presentation_hint,
                variables_reference: w.variables_reference,
                named_variables: None,
                indexed_variables: None,
                expensive: w.expensive,
                line: w.line,
                column: w.column,
                end_line: None,
                end_column: None,
                source: None,
            })
            .collect())
    }

    pub(crate) async fn variables(
        &mut self,
        core_index: usize,
        variables_reference: u32,
        filter: Option<String>,
    ) -> Result<Vec<Variable>, Error> {
        let session = self.session_interface();
        let wire = session
            .variables(core_index as u32, variables_reference, filter)
            .await
            .map_err(rpc_err)?;
        Ok(wire
            .into_iter()
            .map(|w| Variable {
                name: w.name,
                evaluate_name: w.evaluate_name,
                memory_reference: w.memory_reference,
                indexed_variables: w.indexed_variables,
                named_variables: w.named_variables,
                presentation_hint: None,
                type_: w.type_,
                value: w.value,
                variables_reference: w.variables_reference,
                declaration_location_reference: None,
                value_location_reference: None,
            })
            .collect())
    }

    fn evaluate_response_body(
        wire: probe_rs_rpc::debug_vars::WireEvaluateResponse,
    ) -> EvaluateResponseBody {
        EvaluateResponseBody {
            result: wire.result,
            type_: wire.type_,
            variables_reference: wire.variables_reference,
            named_variables: wire.named_variables,
            indexed_variables: wire.indexed_variables,
            memory_reference: wire.memory_reference,
            presentation_hint: None,
            value_location_reference: None,
        }
    }

    pub(crate) async fn evaluate(
        &mut self,
        core_index: usize,
        arguments: &EvaluateArguments,
    ) -> Result<EvaluateResponseBody, Error> {
        let wire = self
            .session_interface()
            .evaluate(
                core_index as u32,
                arguments.frame_id.map(|id| id as u32),
                arguments.expression.clone(),
            )
            .await
            .map_err(rpc_err)?;
        Ok(Self::evaluate_response_body(wire))
    }

    /// Resolve one REPL variable against the server-owned cache.
    pub(crate) async fn evaluate_repl_variable(
        &mut self,
        core_index: usize,
        frame_id: u32,
        expression: String,
    ) -> Result<EvaluateResponseBody, Error> {
        let wire = self
            .session_interface()
            .evaluate(core_index as u32, Some(frame_id), expression)
            .await
            .map_err(rpc_err)?;
        Ok(Self::evaluate_response_body(wire))
    }

    pub(crate) async fn flash_binary_resolved(
        &mut self,
        upload: &ResolvedUpload,
        config: &FlashingConfig,
        rtt_client: Option<Key<RttClient>>,
        progress: &mut dyn FnMut(WireProgressEvent),
    ) -> Result<(), DebuggerError> {
        let session = self.session_interface();

        let build_result = session
            .build_flash_loader_resolved(
                upload,
                config.format_options.clone(),
                None,
                false,
                rtt_client,
            )
            .await
            .map_err(|e| DebuggerError::Other(anyhow::anyhow!(e)))?;

        let loader_key = build_result.loader;

        // Clear the control block of the program that ran before, so that its
        // header does not leak into the first poll cycle. The build above told
        // the client whether this image writes the block itself, in which case
        // the clear is skipped.
        if let Some(rtt_client) = rtt_client {
            session
                .clear_rtt_control_block(rtt_client)
                .await
                .map_err(|e| DebuggerError::Other(anyhow::anyhow!(e)))?;
        }

        let run_flash = if config.verify_before_flashing {
            match session
                .verify(loader_key, async |event| {
                    progress(event);
                })
                .await
                .map_err(|e| DebuggerError::Other(anyhow::anyhow!(e)))?
            {
                VerifyResult::Ok => false,
                VerifyResult::Mismatch => true,
            }
        } else {
            true
        };

        if run_flash {
            let options = WireDownloadOptions {
                keep_unwritten_bytes: config.restore_unwritten_bytes,
                do_chip_erase: config.full_chip_erase,
                skip_erase: false,
                verify: config.verify_after_flashing,
                disable_double_buffering: false,
                preferred_algos: Vec::new(),
                ram_chunk_size: None,
            };

            session
                .flash(options, loader_key, async |event| {
                    progress(event);
                })
                .await
                .map_err(|e| DebuggerError::Other(anyhow::anyhow!(e)))?;
        }

        Ok(())
    }
}

impl From<WireSessionTargetMetadata> for SessionTargetMetadata {
    fn from(wire: WireSessionTargetMetadata) -> Self {
        Self {
            target_name: wire.target_name,
            default_format: wire.default_format,
            cores: wire
                .cores
                .into_iter()
                .map(|core| (core.index as usize, from_wire_core_type(core.core_type)))
                .collect(),
        }
    }
}

impl From<WireSource> for Source {
    fn from(s: WireSource) -> Self {
        Source {
            name: s.name,
            path: s.path,
            source_reference: None,
            presentation_hint: None,
            origin: None,
            sources: None,
            adapter_data: None,
            checksums: None,
        }
    }
}

impl From<WireDisassembledInstruction> for DisassembledInstruction {
    fn from(i: WireDisassembledInstruction) -> Self {
        DisassembledInstruction {
            address: i.address,
            column: i.column,
            end_column: None,
            end_line: None,
            instruction: i.instruction,
            instruction_bytes: i.instruction_bytes,
            line: i.line,
            location: i.location.map(Source::from),
            symbol: None,
            presentation_hint: None,
        }
    }
}

#[cfg(test)]
mod test {
    use probe_rs::{Architecture, CoreRegisters, CoreType, RegisterRole};

    use super::CorePerAttachInfo;
    use probe_rs_rpc::core_ops::WireCoreMetadata;

    #[test]
    fn live_fpu_metadata_selects_floating_point_registers() {
        let info = CorePerAttachInfo::from_wire(
            Architecture::Arm,
            WireCoreMetadata {
                fpu_support: true,
                floating_point_register_count: Some(32),
                instruction_set: probe_rs_rpc::core_ops::WireInstructionSet::Thumb2,
            },
        );

        let registers = CoreRegisters::for_core_type(
            CoreType::Armv7em,
            info.fpu_support,
            info.fp_register_count,
        );

        assert_eq!(info.fp_register_count, Some(32));
        assert!(
            registers
                .all_registers()
                .any(|register| register.register_has_role(RegisterRole::FloatingPoint))
        );
    }

    #[test]
    fn absent_fpu_metadata_selects_core_registers_only() {
        let info = CorePerAttachInfo::from_wire(
            Architecture::Arm,
            WireCoreMetadata {
                fpu_support: false,
                floating_point_register_count: None,
                instruction_set: probe_rs_rpc::core_ops::WireInstructionSet::Thumb2,
            },
        );

        let registers = CoreRegisters::for_core_type(
            CoreType::Armv7em,
            info.fpu_support,
            info.fp_register_count,
        );

        assert!(
            registers
                .all_registers()
                .all(|register| !register.register_has_role(RegisterRole::FloatingPoint))
        );
    }

    #[test]
    fn wire_session_core_converts_to_backend_core_list() {
        use super::SessionTargetMetadata;
        use probe_rs_rpc::info::{WireSessionCore, WireSessionTargetMetadata};

        let wire = WireSessionTargetMetadata {
            target_name: "stm32f407vgtx".to_string(),
            default_format: Some("elf".to_string()),
            cores: vec![
                WireSessionCore {
                    index: 0,
                    core_type: probe_rs_rpc::core_ops::WireCoreType::Armv7em,
                },
                WireSessionCore {
                    index: 1,
                    core_type: probe_rs_rpc::core_ops::WireCoreType::Armv7em,
                },
            ],
            memory_map: vec![],
            flash_sectors: vec![],
        };

        let metadata = SessionTargetMetadata::from(wire);

        assert_eq!(metadata.cores.len(), 2);
        assert_eq!(metadata.target_name, "stm32f407vgtx");
        assert_eq!(metadata.default_format.as_deref(), Some("elf"));
    }
}
