//! RPC endpoints for [`probe_rs::Core`] operations.

use std::num::NonZeroU32;
use std::ops::Range;
use std::time::Duration;

use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::util::rtt::DataFormat;

use crate::rpc::{
    Key,
    functions::{NoResponse, RpcContext, RpcError, RpcResult},
};

/// Common request fields for addressing a single core.
#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct CoreAccessRequest {
    pub sessid: Key<Session>,
    pub core: u32,
}

/// Live core properties needed to select the matching static register file.
#[derive(Debug, Serialize, Deserialize, Schema, Clone, Copy, PartialEq, Eq)]
pub struct WireCoreMetadata {
    pub fpu_support: bool,
    pub floating_point_register_count: Option<u64>,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct CoreHaltRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub timeout: Duration,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct CoreWriteRegRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub id: WireRegisterId,
    pub value: WireRegisterValue,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct CoreReadRegistersRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub ids: Vec<WireRegisterId>,
}

/// One entry in a bulk-register-read response.
///
/// Per-register failures are reported in place so that a single unreadable
/// register does not abort the whole request (reading "all" registers
/// typically touches a few that are context-dependent, e.g. FP registers on
/// a core without FPU enabled).
#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct WireRegisterReadResult {
    pub id: WireRegisterId,
    pub result: Result<WireRegisterValue, RpcError>,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct CoreBreakpointsRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub addresses: Vec<u64>,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct CoreVectorCatchRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub condition: WireVectorCatchCondition,
}

// -- wire types ---------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq, Eq)]
pub struct WireRegisterId(pub u16);

#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq)]
pub enum WireRegisterValue {
    U32(u32),
    U64(u64),
    U128(u128),
}

#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone)]
pub struct WireCoreInformation {
    pub pc: u64,
}

#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq, Eq)]
pub enum WireCoreStatus {
    Running,
    Halted(WireHaltReason),
    LockedUp,
    Sleeping,
    Unknown,
}

#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq, Eq)]
pub enum WireHaltReason {
    Multiple,
    Breakpoint(WireBreakpointCause),
    Exception,
    Watchpoint,
    Step,
    Request,
    External,
    Unknown,
}

/// Reduced breakpoint cause that does not embed the full
/// [`SemihostingCommand`] payload. The DAP backend only needs to know that a
/// semihosting halt occurred; the server handles the command via the
/// monitor/event channels.
#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq, Eq)]
pub enum WireBreakpointCause {
    Hardware,
    Software,
    Unknown,
    /// The target requested a semihosting operation. The operation kind is
    /// serialized as an opcode; parameters stay on the server side and are
    /// surfaced through the existing semihosting event channels when needed.
    Semihosting(WireSemihostingCommand),
}

/// Classification of a semihosting command carried over the wire.
///
/// The full [`SemihostingCommand`] payload carries pointers into target
/// memory and cannot be transported over RPC on its own. We specialise the
/// variants the DAP backend actually needs on the client side:
///
/// * [`Self::ExitSuccess`] / [`Self::ExitError`] reproduce the user-visible
///   "Application has exited with …" message.
/// * [`Self::GetCommandLine`] carries the target address of the command-line
///   block so the client can reconstruct a real
///   [`probe_rs::semihosting::GetCommandLineRequest`] (via
///   [`Buffer::from_block_at`](probe_rs::semihosting::Buffer::from_block_at))
///   and drive the `write_command_line_to_target` handshake through the
///   regular [`probe_rs::CoreInterface`] / memory RPCs.
///
/// Everything else is surfaced as [`Self::Other`]; the server still handles
/// its target-memory interactions locally.
#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq, Eq)]
pub enum WireSemihostingCommand {
    ExitSuccess,
    ExitError(WireExitErrorDetails),
    GetCommandLine { block_address: u32 },
    Other,
}

/// Plain-old-data copy of [`ExitErrorDetails`] for the wire.
#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq, Eq)]
pub struct WireExitErrorDetails {
    pub reason: u32,
    pub exit_status: Option<u32>,
    pub subcode: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq, Eq)]
pub enum WireVectorCatchCondition {
    HardFault,
    CoreReset,
    SecureFault,
    All,
    Svc,
    Hlt,
}

#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq, Eq)]
pub enum WireInstructionSet {
    Thumb2,
    A32,
    A64,
    RV32,
    RV32C,
    RV64,
    RV64C,
    Xtensa,
}

#[derive(Serialize, Deserialize, Schema, Clone, Copy)]
pub enum WireSteppingMode {
    StepInstruction,
    OverStatement,
    IntoStatement,
    OutOfStatement,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct StepRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub mode: WireSteppingMode,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct StepResponse {
    pub status: WireCoreStatus,
    pub program_counter: u64,
    pub warning: Option<String>,
}

pub type StepResult = RpcResult<StepResponse>;

// -- core dump ---------------------------------------------------------------

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct CoreDumpRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub ranges: Vec<Range<u64>>,
}

/// Wire form of [`probe_rs::CoreDump`]. The client reconstructs a `CoreDump`
/// from these fields.
#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct WireCoreDump {
    pub registers: Vec<(WireRegisterId, WireRegisterValue)>,
    pub data: Vec<(Range<u64>, Vec<u8>)>,
    pub instruction_set: WireInstructionSet,
    pub supports_native_64bit_access: bool,
    pub core_type: WireCoreType,
    pub fpu_support: bool,
    pub floating_point_register_count: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Schema, Clone, Copy, PartialEq, Eq)]
pub enum WireCoreType {
    Armv6m,
    Armv7a,
    Armv7r,
    Armv7m,
    Armv7em,
    Armv8a,
    Armv8m,
    Riscv,
    Riscv64,
    Xtensa,
}

// -- semihosting -------------------------------------------------------------

/// UI event produced by server-side semihosting handling, to be replayed on
/// the client so the DAP UI behaves as if the call ran locally.
#[derive(Serialize, Deserialize, Schema, Clone)]
pub enum WireSemihostingUiEvent {
    /// Open an RTT window for a newly-allocated semihosting file handle.
    RttWindow {
        handle: u32,
        path: String,
        format: DataFormat,
    },
    /// Write a line to the DAP console.
    LogToConsole(String),
    /// Emit RTT output for a previously-opened handle.
    RttOutput { handle: u32, data: String },
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct HandleSemihostingRequest {
    pub sessid: Key<Session>,
    pub core: u32,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct HandleSemihostingResult {
    pub status: WireCoreStatus,
    pub events: Vec<WireSemihostingUiEvent>,
}

pub type HandleSemihostingResponse = RpcResult<HandleSemihostingResult>;

// -- handlers -----------------------------------------------------------------

use probe_rs::{
    BreakpointCause, CoreDump, CoreStatus, HaltReason, RegisterId, RegisterValue, Session,
    VectorCatchCondition, semihosting::SemihostingCommand,
};
use probe_rs_debug::{DebugError, SteppingMode};

use crate::rpc::debug_state::{CoreSemihostingState, SemihostingFile};

macro_rules! with_core {
    ($ctx:expr, $sessid:expr, $core:expr, |$core_var:ident| $body:block) => {{
        let mut session = $ctx.session($sessid).await;
        let mut $core_var = session.core($core as usize)?;
        let result: Result<_, probe_rs::Error> = $body;
        result
    }};
}

pub async fn core_status(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> RpcResult<WireCoreStatus> {
    let status = with_core!(ctx, request.sessid, request.core, |core| { core.status() })?;
    Ok(status.into())
}

pub async fn core_halt(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreHaltRequest,
) -> RpcResult<WireCoreInformation> {
    let info = with_core!(ctx, request.sessid, request.core, |core| {
        core.halt(request.timeout)
    })?;
    Ok(info.into())
}

pub async fn core_run(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> NoResponse {
    with_core!(ctx, request.sessid, request.core, |core| { core.run() })?;
    Ok(())
}

/// Full `SteppingMode::step` (over/into/out/instruction) run server-side
/// against the cached `DebugInfo` and the live `Core`. On
/// `WarnAndContinue`, re-reads status/pc and surfaces the warning; other
/// errors halt the core and propagate.
pub async fn core_step(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: StepRequest,
) -> StepResult {
    let debug_info = ctx
        .with_server_debug_state(request.sessid, |state| state.debug_info.clone())
        .await;

    let mut session = ctx.session(request.sessid).await;
    let mut core = session.core(request.core as usize)?;

    let stepping_mode = SteppingMode::from(request.mode);
    let debug_info_ref = debug_info.as_deref();
    match stepping_mode.step(&mut core, debug_info_ref) {
        Ok((status, pc)) => Ok(StepResponse {
            status: status.into(),
            program_counter: pc,
            warning: None,
        }),
        Err(DebugError::WarnAndContinue { message }) => {
            let status = core.status()?;
            let pc: u64 = core
                .read_core_reg::<RegisterValue>(core.program_counter().id())?
                .try_into()?;
            Ok(StepResponse {
                status: status.into(),
                program_counter: pc,
                warning: Some(message),
            })
        }
        Err(other) => {
            core.halt(Duration::from_millis(100)).ok();
            Err(other.to_string())?
        }
    }
}

pub async fn core_write_reg(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreWriteRegRequest,
) -> NoResponse {
    let id: RegisterId = request.id.into();
    let value: RegisterValue = request.value.into();
    with_core!(ctx, request.sessid, request.core, |core| {
        core.write_core_reg(id, value)?;
        Ok(())
    })?;
    Ok(())
}

/// Set a batch of hardware breakpoints.
///
/// Per-address failures are reported in place so that one address that
/// cannot be covered (e.g. no breakpoint units left) does not abort the
/// whole batch. Duplicate addresses within a request succeed after the
/// first.
pub async fn core_set_hw_bps(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreBreakpointsRequest,
) -> RpcResult<Vec<Result<(), RpcError>>> {
    let results = with_core!(ctx, request.sessid, request.core, |core| {
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut out: Vec<Result<(), RpcError>> = Vec::with_capacity(request.addresses.len());
        for address in &request.addresses {
            let result = if seen.contains(address) {
                Ok(())
            } else {
                match core.set_hw_breakpoint(*address) {
                    Ok(()) => {
                        seen.insert(*address);
                        Ok(())
                    }
                    Err(error) => Err(RpcError::from(error)),
                }
            };
            out.push(result);
        }
        Ok(out)
    })?;
    Ok(results)
}

pub async fn core_clear_hw_bps(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreBreakpointsRequest,
) -> NoResponse {
    with_core!(ctx, request.sessid, request.core, |core| {
        for address in request.addresses {
            core.clear_hw_breakpoint(address).or_else(|e| match e {
                probe_rs::Error::BreakpointOperation(probe_rs::BreakpointError::NotFound(_)) => {
                    Ok(())
                }
                e => Err(e),
            })?;
        }
        Ok(())
    })?;
    Ok(())
}

pub async fn core_enable_vc(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreVectorCatchRequest,
) -> NoResponse {
    let cond: VectorCatchCondition = request.condition.into();
    with_core!(ctx, request.sessid, request.core, |core| {
        core.enable_vector_catch(cond)?;
        Ok(())
    })?;
    Ok(())
}

pub async fn core_metadata(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> RpcResult<WireCoreMetadata> {
    let metadata = with_core!(ctx, request.sessid, request.core, |core| {
        let fpu_support = core.fpu_support()?;
        let floating_point_register_count = fpu_support
            .then(|| core.floating_point_register_count())
            .transpose()?
            .map(|count| count as u64);

        Ok(WireCoreMetadata {
            fpu_support,
            floating_point_register_count,
        })
    })?;
    Ok(metadata)
}

/// Bulk-read a set of registers in one request.
///
/// Per-register errors are reported in the matching slot so that an
/// unreadable register (e.g. an FP register on a core with the FPU disabled)
/// does not abort the whole batch.
pub async fn core_read_registers(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreReadRegistersRequest,
) -> RpcResult<Vec<WireRegisterReadResult>> {
    let ids: Vec<RegisterId> = request.ids.iter().copied().map(Into::into).collect();
    let values = with_core!(ctx, request.sessid, request.core, |core| {
        let mut out: Vec<Result<RegisterValue, RpcError>> = Vec::with_capacity(ids.len());
        for id in &ids {
            out.push(
                core.read_core_reg::<RegisterValue>(*id)
                    .map_err(RpcError::from),
            );
        }
        Ok(out)
    })?;

    Ok(request
        .ids
        .into_iter()
        .zip(values)
        .map(|(id, result)| WireRegisterReadResult {
            id,
            result: result.map(Into::into),
        })
        .collect())
}

pub async fn core_dump(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreDumpRequest,
) -> RpcResult<WireCoreDump> {
    let mut session = ctx.session(request.sessid).await;
    let mut core = session.core(request.core as usize)?;

    let dump = CoreDump::dump_core(&mut core, request.ranges)?;

    Ok(WireCoreDump {
        registers: dump
            .registers
            .into_iter()
            .map(|(id, v)| (id.into(), v.into()))
            .collect(),
        data: dump.data,
        instruction_set: dump.instruction_set.into(),
        supports_native_64bit_access: dump.supports_native_64bit_access,
        core_type: dump.core_type.into(),
        fpu_support: dump.fpu_support,
        floating_point_register_count: dump.floating_point_register_count.map(|c| c as u64),
    })
}

/// Read the core status server-side; if it halted on a semihosting command,
/// perform the file I/O next to the target, mutating the server-owned
/// per-core semihosting state, and return the resulting [`CoreStatus`] plus
/// the UI events the client must replay.
pub async fn core_handle_semihosting(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: HandleSemihostingRequest,
) -> HandleSemihostingResponse {
    let states = ctx.debug_states();
    let mut guard = states.lock().await;

    let mut session = ctx.session(request.sessid).await;
    let mut core = session.core(request.core as usize)?;

    let status = core.status()?;
    let command = match status {
        CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(c))) => Some(c),
        _ => None,
    };
    let Some(command) = command else {
        return Ok(HandleSemihostingResult {
            status: status.into(),
            events: vec![],
        });
    };

    let Some(state) = guard.get_mut(&request.sessid) else {
        Err("No debug state for session")?
    };
    let sh = state.semihosting_state(request.core as usize);

    let mut events = Vec::new();
    let result = handle_semihosting_impl(&mut core, sh, command, &mut events)?;
    Ok(HandleSemihostingResult {
        status: result.into(),
        events,
    })
}

fn handle_semihosting_impl(
    core: &mut probe_rs::Core,
    sh: &mut CoreSemihostingState,
    command: SemihostingCommand,
    events: &mut Vec<WireSemihostingUiEvent>,
) -> Result<CoreStatus, probe_rs::Error> {
    match command {
        SemihostingCommand::Open(request) => {
            tracing::debug!("Semihosting request: open {request:?}");
            let path = request.path(core)?;
            let mode = request.mode();

            let is_write = mode.starts_with('w') || mode.starts_with('a');
            let is_append = mode.starts_with('a');
            let is_stdio = path == ":tt";

            let path = if is_stdio {
                if is_append { "stderr" } else { "stdout" }.to_string()
            } else {
                path
            };

            let is_binary = mode.ends_with('b');
            let format = if is_binary {
                DataFormat::BinaryLE
            } else {
                DataFormat::String
            };

            if is_write {
                if let Some(file) = sh.handles.values().find(|f| f.path == path) {
                    request.respond_with_handle(core, file.handle)?;
                } else {
                    let handle = sh.next_handle;
                    #[expect(clippy::unwrap_used, reason = "Infallible from 1024")]
                    let nz_handle = NonZeroU32::new(handle).unwrap();
                    sh.handles.insert(
                        handle,
                        SemihostingFile {
                            handle: nz_handle,
                            path: path.clone(),
                            mode,
                        },
                    );
                    sh.next_handle += 1;

                    events.push(WireSemihostingUiEvent::RttWindow {
                        handle,
                        path,
                        format,
                    });
                    request.respond_with_handle(core, nz_handle)?;
                }
            }
        }
        SemihostingCommand::Close(request) => {
            tracing::debug!("Semihosting request: close {request:?}");
            request.success(core)?;
        }
        SemihostingCommand::WriteConsole(request) => {
            tracing::debug!("Semihosting request: write console {request:?}");
            let string = request.read(core)?;
            events.push(WireSemihostingUiEvent::LogToConsole(string));
        }
        SemihostingCommand::Write(request) => {
            tracing::debug!("Semihosting request: write {request:?}");
            let handle = request.file_handle();
            let bytes = request.read(core)?;

            if let Some(file) = sh.handles.get(&handle) {
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

                events.push(WireSemihostingUiEvent::RttOutput { handle, data });
                request.write_status(core, 0)?;
            }
        }
        SemihostingCommand::Errno(request) => {
            request.write_errno(core, 0)?;
        }

        SemihostingCommand::ExitSuccess => {
            events.push(WireSemihostingUiEvent::LogToConsole(
                "Application has exited with success.".to_string(),
            ));
            return Ok(CoreStatus::Halted(HaltReason::Breakpoint(
                BreakpointCause::Semihosting(SemihostingCommand::ExitSuccess),
            )));
        }
        SemihostingCommand::ExitError(details) => {
            events.push(WireSemihostingUiEvent::LogToConsole(format!(
                "Application has exited with {details}"
            )));
            return Ok(CoreStatus::Halted(HaltReason::Breakpoint(
                BreakpointCause::Semihosting(SemihostingCommand::ExitError(details)),
            )));
        }

        unhandled => {
            tracing::warn!("Unhandled semihosting command: {:?}", unhandled);
            return Ok(CoreStatus::Halted(HaltReason::Breakpoint(
                BreakpointCause::Semihosting(unhandled),
            )));
        }
    };

    core.run()?;
    Ok(CoreStatus::Running)
}

pub(crate) mod convert {
    use super::{
        WireBreakpointCause, WireCoreInformation, WireCoreStatus, WireCoreType,
        WireExitErrorDetails, WireHaltReason, WireInstructionSet, WireRegisterId,
        WireRegisterValue, WireSemihostingCommand, WireSteppingMode, WireVectorCatchCondition,
    };
    use probe_rs::{
        CoreInformation, CoreStatus, HaltReason, InstructionSet, RegisterId, RegisterValue,
        VectorCatchCondition,
        semihosting::{ExitErrorDetails, SemihostingCommand, UnknownCommandDetails},
    };
    use probe_rs_debug::SteppingMode;

    impl From<RegisterId> for WireRegisterId {
        fn from(value: RegisterId) -> Self {
            WireRegisterId(value.0)
        }
    }

    impl From<WireRegisterId> for RegisterId {
        fn from(value: WireRegisterId) -> Self {
            RegisterId(value.0)
        }
    }

    impl From<RegisterValue> for WireRegisterValue {
        fn from(value: RegisterValue) -> Self {
            match value {
                RegisterValue::U32(v) => WireRegisterValue::U32(v),
                RegisterValue::U64(v) => WireRegisterValue::U64(v),
                RegisterValue::U128(v) => WireRegisterValue::U128(v),
            }
        }
    }

    impl From<WireRegisterValue> for RegisterValue {
        fn from(value: WireRegisterValue) -> Self {
            match value {
                WireRegisterValue::U32(v) => RegisterValue::U32(v),
                WireRegisterValue::U64(v) => RegisterValue::U64(v),
                WireRegisterValue::U128(v) => RegisterValue::U128(v),
            }
        }
    }

    impl From<CoreInformation> for WireCoreInformation {
        fn from(value: CoreInformation) -> Self {
            Self { pc: value.pc }
        }
    }

    impl From<WireCoreInformation> for CoreInformation {
        fn from(value: WireCoreInformation) -> Self {
            Self { pc: value.pc }
        }
    }

    impl From<&ExitErrorDetails> for WireExitErrorDetails {
        fn from(value: &ExitErrorDetails) -> Self {
            Self {
                reason: value.reason,
                exit_status: value.exit_status,
                subcode: value.subcode,
            }
        }
    }

    impl From<WireExitErrorDetails> for ExitErrorDetails {
        fn from(value: WireExitErrorDetails) -> Self {
            Self {
                reason: value.reason,
                exit_status: value.exit_status,
                subcode: value.subcode,
            }
        }
    }

    impl From<&SemihostingCommand> for WireSemihostingCommand {
        fn from(value: &SemihostingCommand) -> Self {
            match value {
                SemihostingCommand::ExitSuccess => WireSemihostingCommand::ExitSuccess,
                SemihostingCommand::ExitError(details) => {
                    WireSemihostingCommand::ExitError(details.into())
                }
                SemihostingCommand::GetCommandLine(request) => {
                    WireSemihostingCommand::GetCommandLine {
                        block_address: request.block_address(),
                    }
                }
                _ => WireSemihostingCommand::Other,
            }
        }
    }

    impl From<CoreStatus> for WireCoreStatus {
        fn from(value: CoreStatus) -> Self {
            match value {
                CoreStatus::Running => WireCoreStatus::Running,
                CoreStatus::Halted(reason) => WireCoreStatus::Halted(reason.into()),
                CoreStatus::LockedUp => WireCoreStatus::LockedUp,
                CoreStatus::Sleeping => WireCoreStatus::Sleeping,
                CoreStatus::Unknown => WireCoreStatus::Unknown,
            }
        }
    }

    impl From<HaltReason> for WireHaltReason {
        fn from(value: HaltReason) -> Self {
            use probe_rs::BreakpointCause;
            match value {
                HaltReason::Multiple => WireHaltReason::Multiple,
                HaltReason::Breakpoint(cause) => WireHaltReason::Breakpoint(match cause {
                    BreakpointCause::Hardware => WireBreakpointCause::Hardware,
                    BreakpointCause::Software => WireBreakpointCause::Software,
                    BreakpointCause::Unknown => WireBreakpointCause::Unknown,
                    BreakpointCause::Semihosting(ref cmd) => {
                        WireBreakpointCause::Semihosting(cmd.into())
                    }
                }),
                HaltReason::Exception => WireHaltReason::Exception,
                HaltReason::Watchpoint => WireHaltReason::Watchpoint,
                HaltReason::Step => WireHaltReason::Step,
                HaltReason::Request => WireHaltReason::Request,
                HaltReason::External => WireHaltReason::External,
                HaltReason::Unknown => WireHaltReason::Unknown,
            }
        }
    }

    // `WireHaltReason` cannot round-trip perfectly because the general semihosting
    // payload carries target-memory pointers. The exit / exit-error classification
    // (and exit status / reason codes) is preserved so the DAP server can emit the
    // same "Application has exited with …" message on both the local and remote
    // paths.
    //
    // `GetCommandLine` is surfaced as a placeholder `SemihostingCommand::Unknown`;
    // the server-side `core/handle_semihosting` endpoint re-derives the real
    // command from the live core, so the client never needs target memory access
    // for it.
    impl From<WireBreakpointCause> for probe_rs::BreakpointCause {
        fn from(value: WireBreakpointCause) -> Self {
            match value {
                WireBreakpointCause::Hardware => probe_rs::BreakpointCause::Hardware,
                WireBreakpointCause::Software => probe_rs::BreakpointCause::Software,
                WireBreakpointCause::Unknown => probe_rs::BreakpointCause::Unknown,
                WireBreakpointCause::Semihosting(cmd) => {
                    probe_rs::BreakpointCause::Semihosting(match cmd {
                        WireSemihostingCommand::ExitSuccess => SemihostingCommand::ExitSuccess,
                        WireSemihostingCommand::ExitError(details) => {
                            SemihostingCommand::ExitError(details.into())
                        }
                        // Placeholder: the server-side `core/handle_semihosting`
                        // endpoint re-derives the real `GetCommandLineRequest`
                        // from the live core.
                        WireSemihostingCommand::GetCommandLine { .. } => {
                            SemihostingCommand::Unknown(UnknownCommandDetails {
                                operation: 0,
                                parameter: 0,
                            })
                        }
                        // The server handles the real command payload; surface a
                        // placeholder so the DAP server recognizes the halt as
                        // semihosting-induced.
                        WireSemihostingCommand::Other => {
                            SemihostingCommand::Unknown(UnknownCommandDetails {
                                operation: 0,
                                parameter: 0,
                            })
                        }
                    })
                }
            }
        }
    }

    impl From<WireHaltReason> for HaltReason {
        fn from(value: WireHaltReason) -> Self {
            match value {
                WireHaltReason::Multiple => HaltReason::Multiple,
                WireHaltReason::Breakpoint(cause) => HaltReason::Breakpoint(cause.into()),
                WireHaltReason::Exception => HaltReason::Exception,
                WireHaltReason::Watchpoint => HaltReason::Watchpoint,
                WireHaltReason::Step => HaltReason::Step,
                WireHaltReason::Request => HaltReason::Request,
                WireHaltReason::External => HaltReason::External,
                WireHaltReason::Unknown => HaltReason::Unknown,
            }
        }
    }

    impl From<WireCoreStatus> for CoreStatus {
        fn from(value: WireCoreStatus) -> Self {
            match value {
                WireCoreStatus::Running => CoreStatus::Running,
                WireCoreStatus::Halted(reason) => CoreStatus::Halted(reason.into()),
                WireCoreStatus::LockedUp => CoreStatus::LockedUp,
                WireCoreStatus::Sleeping => CoreStatus::Sleeping,
                WireCoreStatus::Unknown => CoreStatus::Unknown,
            }
        }
    }

    impl From<VectorCatchCondition> for WireVectorCatchCondition {
        fn from(value: VectorCatchCondition) -> Self {
            match value {
                VectorCatchCondition::HardFault => WireVectorCatchCondition::HardFault,
                VectorCatchCondition::CoreReset => WireVectorCatchCondition::CoreReset,
                VectorCatchCondition::SecureFault => WireVectorCatchCondition::SecureFault,
                VectorCatchCondition::All => WireVectorCatchCondition::All,
                VectorCatchCondition::Svc => WireVectorCatchCondition::Svc,
                VectorCatchCondition::Hlt => WireVectorCatchCondition::Hlt,
            }
        }
    }

    impl From<WireVectorCatchCondition> for VectorCatchCondition {
        fn from(value: WireVectorCatchCondition) -> Self {
            match value {
                WireVectorCatchCondition::HardFault => VectorCatchCondition::HardFault,
                WireVectorCatchCondition::CoreReset => VectorCatchCondition::CoreReset,
                WireVectorCatchCondition::SecureFault => VectorCatchCondition::SecureFault,
                WireVectorCatchCondition::All => VectorCatchCondition::All,
                WireVectorCatchCondition::Svc => VectorCatchCondition::Svc,
                WireVectorCatchCondition::Hlt => VectorCatchCondition::Hlt,
            }
        }
    }

    impl From<InstructionSet> for WireInstructionSet {
        fn from(value: InstructionSet) -> Self {
            match value {
                InstructionSet::Thumb2 => WireInstructionSet::Thumb2,
                InstructionSet::A32 => WireInstructionSet::A32,
                InstructionSet::A64 => WireInstructionSet::A64,
                InstructionSet::RV32 => WireInstructionSet::RV32,
                InstructionSet::RV32C => WireInstructionSet::RV32C,
                InstructionSet::RV64 => WireInstructionSet::RV64,
                InstructionSet::RV64C => WireInstructionSet::RV64C,
                InstructionSet::Xtensa => WireInstructionSet::Xtensa,
            }
        }
    }

    impl From<WireInstructionSet> for InstructionSet {
        fn from(value: WireInstructionSet) -> Self {
            match value {
                WireInstructionSet::Thumb2 => InstructionSet::Thumb2,
                WireInstructionSet::A32 => InstructionSet::A32,
                WireInstructionSet::A64 => InstructionSet::A64,
                WireInstructionSet::RV32 => InstructionSet::RV32,
                WireInstructionSet::RV32C => InstructionSet::RV32C,
                WireInstructionSet::RV64 => InstructionSet::RV64,
                WireInstructionSet::RV64C => InstructionSet::RV64C,
                WireInstructionSet::Xtensa => InstructionSet::Xtensa,
            }
        }
    }

    impl From<WireSteppingMode> for SteppingMode {
        fn from(mode: WireSteppingMode) -> Self {
            match mode {
                WireSteppingMode::StepInstruction => SteppingMode::StepInstruction,
                WireSteppingMode::OverStatement => SteppingMode::OverStatement,
                WireSteppingMode::IntoStatement => SteppingMode::IntoStatement,
                WireSteppingMode::OutOfStatement => SteppingMode::OutOfStatement,
            }
        }
    }

    impl From<probe_rs::CoreType> for WireCoreType {
        fn from(value: probe_rs::CoreType) -> Self {
            match value {
                probe_rs::CoreType::Armv6m => WireCoreType::Armv6m,
                probe_rs::CoreType::Armv7a => WireCoreType::Armv7a,
                probe_rs::CoreType::Armv7r => WireCoreType::Armv7r,
                probe_rs::CoreType::Armv7m => WireCoreType::Armv7m,
                probe_rs::CoreType::Armv7em => WireCoreType::Armv7em,
                probe_rs::CoreType::Armv8a => WireCoreType::Armv8a,
                probe_rs::CoreType::Armv8m => WireCoreType::Armv8m,
                probe_rs::CoreType::Riscv => WireCoreType::Riscv,
                probe_rs::CoreType::Riscv64 => WireCoreType::Riscv64,
                probe_rs::CoreType::Xtensa => WireCoreType::Xtensa,
            }
        }
    }

    impl From<WireCoreType> for probe_rs::CoreType {
        fn from(value: WireCoreType) -> Self {
            match value {
                WireCoreType::Armv6m => probe_rs::CoreType::Armv6m,
                WireCoreType::Armv7a => probe_rs::CoreType::Armv7a,
                WireCoreType::Armv7r => probe_rs::CoreType::Armv7r,
                WireCoreType::Armv7m => probe_rs::CoreType::Armv7m,
                WireCoreType::Armv7em => probe_rs::CoreType::Armv7em,
                WireCoreType::Armv8a => probe_rs::CoreType::Armv8a,
                WireCoreType::Armv8m => probe_rs::CoreType::Armv8m,
                WireCoreType::Riscv => probe_rs::CoreType::Riscv,
                WireCoreType::Riscv64 => probe_rs::CoreType::Riscv64,
                WireCoreType::Xtensa => probe_rs::CoreType::Xtensa,
            }
        }
    }
}
