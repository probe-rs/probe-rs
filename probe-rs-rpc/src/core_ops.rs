use std::ops::Range;
use std::time::Duration;

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::{Key, RpcError, RpcResult, Session, rtt_config::DataFormat};

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
    pub instruction_set: WireInstructionSet,
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

#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq, Eq)]
pub struct WireRegisterId(pub u16);

#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq)]
pub enum WireRegisterValue {
    U32(u32),
    U64(u64),
    U128(u128),
}

#[derive(Debug, Serialize, Deserialize, Schema, Copy, Clone, PartialEq)]
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
/// `SemihostingCommand` payload. The DAP backend only needs to know that a
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
/// The full `SemihostingCommand` payload carries pointers into target
/// memory and cannot be transported over RPC on its own. We specialise the
/// variants the DAP backend actually needs on the client side:
///
/// * [`Self::ExitSuccess`] / [`Self::ExitError`] reproduce the user-visible
///   "Application has exited with …" message.
/// * [`Self::GetCommandLine`] carries the target address of the command-line
///   block so the client can reconstruct a real
///   `probe_rs::semihosting::GetCommandLineRequest` (via
///   `Buffer::from_block_at` in `probe_rs::semihosting::Buffer`)
///   and drive the `write_command_line_to_target` handshake through the
///   regular `probe_rs::CoreInterface` / memory RPCs.
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

/// Plain-old-data copy of `ExitErrorDetails` for the wire.
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

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct CoreDumpRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub ranges: Vec<Range<u64>>,
}

/// Wire form of `probe_rs::CoreDump`. The client reconstructs a `CoreDump`
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
