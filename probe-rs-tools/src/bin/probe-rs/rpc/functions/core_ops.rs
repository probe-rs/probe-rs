//! RPC endpoints for [`probe_rs::Core`] operations.
//!
//! The existing memory/reset endpoints only cover a small subset of what a DAP
//! backend needs. This module adds the remaining core-control and introspection
//! endpoints so that a remote DAP server can drive a target through the same
//! [`probe_rs::Core`] API as a local one.

use std::time::Duration;

use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::{
    CoreInformation, CoreStatus, HaltReason, InstructionSet, RegisterId, RegisterValue, Session,
    VectorCatchCondition,
    semihosting::{ExitErrorDetails, SemihostingCommand, UnknownCommandDetails},
};
use serde::{Deserialize, Serialize};

use crate::rpc::{
    Key,
    functions::{NoResponse, RpcContext, RpcResult},
};

/// Common core addressing.
#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct CoreAccessRequest {
    pub sessid: Key<Session>,
    pub core: u32,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct CoreHaltRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub timeout: Duration,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct CoreWaitHaltedRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub timeout: Duration,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct CoreReadRegRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub id: WireRegisterId,
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
/// Per-register failures are surfaced as [`None`] so that a single
/// unreadable register does not abort the whole request (reading "all"
/// registers typically touches a few that are context-dependent, e.g.
/// FP registers on a core without FPU enabled).
#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct WireRegisterReadResult {
    pub id: WireRegisterId,
    pub value: Option<WireRegisterValue>,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct CoreBreakpointRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub address: u64,
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

#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone)]
pub enum WireRegisterValue {
    U32(u32),
    U64(u64),
    U128(u128),
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

#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone)]
pub struct WireCoreInformation {
    pub pc: u64,
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
/// [`SemihostingCommand`] payload. Semihosting commands are still handled by
/// the server via the monitor/event channels; the DAP backend only needs to
/// know that a semihosting halt occurred.
#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq, Eq)]
pub enum WireBreakpointCause {
    Hardware,
    Software,
    Unknown,
    /// The target requested the host to perform a semihosting operation. The
    /// operation kind is serialized as an opcode; parameters stay on the
    /// server side and are surfaced through the existing semihosting event
    /// channels when needed.
    Semihosting(WireSemihostingCommand),
}

/// Classification of a semihosting command carried over the wire.
///
/// The full [`SemihostingCommand`] payload carries pointers into target
/// memory and so cannot be meaningfully transported over RPC on its own.
/// We specialise the variants the DAP backend actually needs to drive on
/// the client side:
///
/// * [`Self::ExitSuccess`] / [`Self::ExitError`] reproduce the
///   user-visible "Application has exited with …" message.
/// * [`Self::GetCommandLine`] carries the target address of the
///   command-line block so the client can reconstruct a real
///   [`probe_rs::semihosting::GetCommandLineRequest`] (via
///   [`Buffer::from_block_at`](probe_rs::semihosting::Buffer::from_block_at))
///   and drive the `write_command_line_to_target` handshake entirely
///   through the regular [`probe_rs::CoreInterface`] / memory RPCs.
///
/// Everything else is surfaced as [`Self::Other`]; the server still
/// handles its target-memory interactions locally.
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

// `WireHaltReason` cannot be round-tripped perfectly because the general
// semihosting payload carries target-memory pointers. We do preserve the
// exit / exit-error classification (and the exit status / reason codes)
// though, so the DAP server can emit the same "Application has exited
// with …" message on both the local and remote paths.
//
// The `GetCommandLine` variant is reconstructed with a zero-initialised
// [`Buffer`] here; the client reissues [`Buffer::from_block_at`] against
// its [`RpcRemoteCore`] in [`RpcRemoteCore::status`] so the request ends
// up bound to the correct target addresses.
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
                    // Placeholder: callers that want a usable
                    // `GetCommandLineRequest` rehydrate one against the
                    // target via `Buffer::from_block_at`. See
                    // `RpcRemoteCore::status`.
                    WireSemihostingCommand::GetCommandLine { .. } => {
                        SemihostingCommand::Unknown(UnknownCommandDetails {
                            operation: 0,
                            parameter: 0,
                        })
                    }
                    // The server handles the real command payload; surface a
                    // placeholder here so the DAP server recognizes the halt as
                    // semihosting-induced without reconstituting the full
                    // operation/parameter pair.
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

#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq, Eq)]
pub enum WireVectorCatchCondition {
    HardFault,
    CoreReset,
    SecureFault,
    All,
    Svc,
    Hlt,
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

// -- handlers -----------------------------------------------------------------

macro_rules! with_core {
    ($ctx:expr, $req:expr, |$core:ident| $body:block) => {{
        let mut session = $ctx.session($req.sessid).await;
        let mut $core = session.core($req.core as usize)?;
        let result: Result<_, probe_rs::Error> = $body;
        result
    }};
}

pub async fn core_status(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> RpcResult<WireCoreStatus> {
    let status = with_core!(ctx, request, |core| { core.status() })?;
    Ok(status.into())
}

pub async fn core_halted(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> RpcResult<bool> {
    let halted = with_core!(ctx, request, |core| { core.core_halted() })?;
    Ok(halted)
}

pub async fn core_wait_halted(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreWaitHaltedRequest,
) -> NoResponse {
    with_core!(
        ctx,
        CoreAccessRequest {
            sessid: request.sessid,
            core: request.core,
        },
        |core| { core.wait_for_core_halted(request.timeout) }
    )?;
    Ok(())
}

pub async fn core_halt(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreHaltRequest,
) -> RpcResult<WireCoreInformation> {
    let info = with_core!(
        ctx,
        CoreAccessRequest {
            sessid: request.sessid,
            core: request.core,
        },
        |core| { core.halt(request.timeout) }
    )?;
    Ok(info.into())
}

pub async fn core_run(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> NoResponse {
    with_core!(ctx, request, |core| { core.run() })?;
    Ok(())
}

pub async fn core_step(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> RpcResult<WireCoreInformation> {
    let info = with_core!(ctx, request, |core| { core.step() })?;
    Ok(info.into())
}

pub async fn core_read_reg(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreReadRegRequest,
) -> RpcResult<WireRegisterValue> {
    let id: RegisterId = request.id.into();
    let value: RegisterValue = with_core!(
        ctx,
        CoreAccessRequest {
            sessid: request.sessid,
            core: request.core,
        },
        |core| { core.read_core_reg::<RegisterValue>(id) }
    )?;
    Ok(value.into())
}

pub async fn core_write_reg(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreWriteRegRequest,
) -> NoResponse {
    let id: RegisterId = request.id.into();
    let value: RegisterValue = request.value.into();
    with_core!(
        ctx,
        CoreAccessRequest {
            sessid: request.sessid,
            core: request.core,
        },
        |core| {
            core.write_core_reg(id, value)?;
            Ok(())
        }
    )?;
    Ok(())
}

pub async fn core_set_hw_bp(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreBreakpointRequest,
) -> NoResponse {
    with_core!(
        ctx,
        CoreAccessRequest {
            sessid: request.sessid,
            core: request.core,
        },
        |core| {
            core.set_hw_breakpoint(request.address)?;
            Ok(())
        }
    )?;
    Ok(())
}

pub async fn core_clear_hw_bp(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreBreakpointRequest,
) -> NoResponse {
    with_core!(
        ctx,
        CoreAccessRequest {
            sessid: request.sessid,
            core: request.core,
        },
        |core| {
            core.clear_hw_breakpoint(request.address)?;
            Ok(())
        }
    )?;
    Ok(())
}

pub async fn core_available_bp_units(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> RpcResult<u32> {
    let n = with_core!(ctx, request, |core| { core.available_breakpoint_units() })?;
    Ok(n)
}

pub async fn core_enable_vc(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreVectorCatchRequest,
) -> NoResponse {
    let cond: VectorCatchCondition = request.condition.into();
    with_core!(
        ctx,
        CoreAccessRequest {
            sessid: request.sessid,
            core: request.core,
        },
        |core| {
            core.enable_vector_catch(cond)?;
            Ok(())
        }
    )?;
    Ok(())
}

pub async fn core_disable_vc(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreVectorCatchRequest,
) -> NoResponse {
    let cond: VectorCatchCondition = request.condition.into();
    with_core!(
        ctx,
        CoreAccessRequest {
            sessid: request.sessid,
            core: request.core,
        },
        |core| {
            core.disable_vector_catch(cond)?;
            Ok(())
        }
    )?;
    Ok(())
}

pub async fn core_instruction_set(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> RpcResult<WireInstructionSet> {
    let iset = with_core!(ctx, request, |core| { core.instruction_set() })?;
    Ok(iset.into())
}

/// Bulk-read a set of registers in one request.
///
/// The primary caller is the RPC-backed DAP backend: on every halt the
/// DAP server performs a register dump (one read per register, from
/// PC/SP/FP/LR through every general-purpose and FP register), which
/// otherwise costs one round-trip per register. Batching them behind a
/// single RPC makes the halt-refresh O(1) round trips instead of
/// O(N_registers).
///
/// Per-register errors are reported as `None` in the matching slot so
/// that an unreadable register (e.g. an FP register on a core that has
/// the FPU disabled) does not abort the whole batch.
pub async fn core_read_registers(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreReadRegistersRequest,
) -> RpcResult<Vec<WireRegisterReadResult>> {
    let ids: Vec<RegisterId> = request.ids.iter().copied().map(Into::into).collect();
    let values = with_core!(
        ctx,
        CoreAccessRequest {
            sessid: request.sessid,
            core: request.core,
        },
        |core| {
            let mut out: Vec<Option<RegisterValue>> = Vec::with_capacity(ids.len());
            for id in &ids {
                out.push(core.read_core_reg::<RegisterValue>(*id).ok());
            }
            Ok(out)
        }
    )?;

    Ok(request
        .ids
        .into_iter()
        .zip(values)
        .map(|(id, value)| WireRegisterReadResult {
            id,
            value: value.map(Into::into),
        })
        .collect())
}
