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
    semihosting::{SemihostingCommand, UnknownCommandDetails},
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

/// Coarse-grained classification of a semihosting command.
///
/// The full [`SemihostingCommand`] payload carries pointers into target
/// memory and so cannot be meaningfully transported over RPC on its own.
/// For the DAP backend, a coarse-grained "exit success / exit error /
/// other" flag is sufficient to drive the UI; the server handles the full
/// semihosting protocol locally and forwards any user-visible output
/// through the existing semihosting event channel.
#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq, Eq)]
pub enum WireSemihostingCommand {
    ExitSuccess,
    ExitError,
    Other,
}

impl From<&SemihostingCommand> for WireSemihostingCommand {
    fn from(value: &SemihostingCommand) -> Self {
        match value {
            SemihostingCommand::ExitSuccess => WireSemihostingCommand::ExitSuccess,
            SemihostingCommand::ExitError(_) => WireSemihostingCommand::ExitError,
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
            HaltReason::Breakpoint(cause) => {
                WireHaltReason::Breakpoint(match cause {
                    BreakpointCause::Hardware => WireBreakpointCause::Hardware,
                    BreakpointCause::Software => WireBreakpointCause::Software,
                    BreakpointCause::Unknown => WireBreakpointCause::Unknown,
                    BreakpointCause::Semihosting(ref cmd) => {
                        WireBreakpointCause::Semihosting(cmd.into())
                    }
                })
            }
            HaltReason::Exception => WireHaltReason::Exception,
            HaltReason::Watchpoint => WireHaltReason::Watchpoint,
            HaltReason::Step => WireHaltReason::Step,
            HaltReason::Request => WireHaltReason::Request,
            HaltReason::External => WireHaltReason::External,
            HaltReason::Unknown => WireHaltReason::Unknown,
        }
    }
}

// `WireHaltReason` cannot be round-tripped back into a full `HaltReason`
// because the `Semihosting` variant loses data on the way. The DAP backend
// only needs to reason about `CoreStatus::is_halted` / `is_running` and a
// coarse-grained breakpoint-cause, so an approximate reverse mapping is fine.
impl From<WireBreakpointCause> for probe_rs::BreakpointCause {
    fn from(value: WireBreakpointCause) -> Self {
        match value {
            WireBreakpointCause::Hardware => probe_rs::BreakpointCause::Hardware,
            WireBreakpointCause::Software => probe_rs::BreakpointCause::Software,
            WireBreakpointCause::Unknown => probe_rs::BreakpointCause::Unknown,
            // Intentionally mapped to `Unknown`: the server keeps the real
            // command payload; the DAP backend surfaces semihosting halts
            // through the dedicated event channel rather than reconstituting a
            // full `SemihostingCommand` over RPC.
            WireBreakpointCause::Semihosting(_) => {
                probe_rs::BreakpointCause::Semihosting(SemihostingCommand::Unknown(
                    UnknownCommandDetails {
                        operation: 0,
                        parameter: 0,
                    },
                ))
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
        let result: Result<_, probe_rs::Error> = (|| $body)();
        result
    }};
}

pub async fn core_status(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> RpcResult<WireCoreStatus> {
    let status = with_core!(ctx, request, |core| { Ok(core.status()?) })?;
    Ok(status.into())
}

pub async fn core_halted(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> RpcResult<bool> {
    let halted = with_core!(ctx, request, |core| { Ok(core.core_halted()?) })?;
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
        |core| { Ok(core.wait_for_core_halted(request.timeout)?) }
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
        |core| { Ok(core.halt(request.timeout)?) }
    )?;
    Ok(info.into())
}

pub async fn core_run(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> NoResponse {
    with_core!(ctx, request, |core| { Ok(core.run()?) })?;
    Ok(())
}

pub async fn core_step(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> RpcResult<WireCoreInformation> {
    let info = with_core!(ctx, request, |core| { Ok(core.step()?) })?;
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
        |core| { Ok(core.read_core_reg(id)?) }
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
    let n = with_core!(ctx, request, |core| { Ok(core.available_breakpoint_units()?) })?;
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
    let iset = with_core!(ctx, request, |core| { Ok(core.instruction_set()?) })?;
    Ok(iset.into())
}
