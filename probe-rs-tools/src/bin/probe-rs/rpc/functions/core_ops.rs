//! RPC endpoints for [`probe_rs::Core`] operations.

use std::num::NonZeroU32;
use std::time::Duration;

use postcard_rpc::header::VarHeader;
use probe_rs_rpc::core_ops::{
    CoreAccessRequest, CoreBreakpointsRequest, CoreDumpRequest, CoreHaltRequest,
    CoreReadRegistersRequest, CoreVectorCatchRequest, CoreWriteRegRequest,
    HandleSemihostingRequest, HandleSemihostingResponse, HandleSemihostingResult, StepRequest,
    StepResponse, StepResult, WireBreakpointCause, WireCoreDump, WireCoreInformation,
    WireCoreMetadata, WireCoreStatus, WireCoreType, WireExitErrorDetails, WireHaltReason,
    WireInstructionSet, WireRegisterId, WireRegisterReadResult, WireRegisterValue,
    WireSemihostingCommand, WireSemihostingUiEvent, WireSteppingMode, WireVectorCatchCondition,
};
use probe_rs_rpc::rtt_config::DataFormat;

use probe_rs::{
    BreakpointCause, CoreDump, CoreStatus, HaltReason, RegisterId, RegisterValue,
    semihosting::SemihostingCommand,
};
use probe_rs_debug::DebugError;

use crate::rpc::debug_state::{CoreSemihostingState, SemihostingFile};
use crate::rpc::functions::{RpcContext, convert::lift};
use probe_rs_rpc::{NoResponse, RpcError, RpcResult};

macro_rules! probe_rs_try {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return Err($crate::rpc::functions::convert::rpc_error_probe_rs(e)),
        }
    };
}

macro_rules! with_core {
    ($ctx:expr, $sessid:expr, $core:expr, |$core_var:ident| $body:block) => {{
        let mut session = $ctx.session($sessid).await;
        let mut $core_var = match session.core($core as usize) {
            Ok(core) => core,
            Err(e) => {
                return Err($crate::rpc::functions::convert::rpc_error_probe_rs(e));
            }
        };
        $body
    }};
}

pub async fn core_status(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> RpcResult<WireCoreStatus> {
    let status = with_core!(ctx, request.sessid, request.core, |core| {
        probe_rs_try!(core.status())
    });
    Ok(convert::to_wire_core_status(status))
}

pub async fn core_halt(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreHaltRequest,
) -> RpcResult<WireCoreInformation> {
    let info = with_core!(ctx, request.sessid, request.core, |core| {
        probe_rs_try!(core.halt(request.timeout))
    });
    Ok(convert::to_wire_core_information(info))
}

pub async fn core_run(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> NoResponse {
    with_core!(ctx, request.sessid, request.core, |core| {
        probe_rs_try!(core.run());
    });
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
    let mut core = lift(session.core(request.core as usize))?;

    let stepping_mode = convert::from_wire_stepping_mode(request.mode);
    let debug_info_ref = debug_info.as_deref();
    match stepping_mode.step(&mut core, debug_info_ref) {
        Ok((status, pc)) => Ok(StepResponse {
            status: convert::to_wire_core_status(status),
            program_counter: pc,
            warning: None,
        }),
        Err(DebugError::WarnAndContinue { message }) => {
            let status = lift(core.status())?;
            let pc: u64 = lift(core.read_core_reg::<RegisterValue>(core.program_counter().id()))?
                .try_into()
                .map_err(|e| {
                    crate::rpc::functions::convert::rpc_error_anyhow_from(anyhow::anyhow!("{e:?}"))
                })?;
            Ok(StepResponse {
                status: convert::to_wire_core_status(status),
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
    let id = convert::from_wire_register_id(request.id);
    let value = convert::from_wire_register_value(request.value);
    with_core!(ctx, request.sessid, request.core, |core| {
        probe_rs_try!(core.write_core_reg(id, value));
    });
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
                    Err(error) => Err(crate::rpc::functions::convert::rpc_error_probe_rs(error)),
                }
            };
            out.push(result);
        }
        out
    });
    Ok(results)
}

pub async fn core_clear_hw_bps(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreBreakpointsRequest,
) -> NoResponse {
    with_core!(ctx, request.sessid, request.core, |core| {
        for address in request.addresses {
            probe_rs_try!(core.clear_hw_breakpoint(address).or_else(|e| match e {
                probe_rs::Error::BreakpointOperation(probe_rs::BreakpointError::NotFound(_)) => {
                    Ok(())
                }
                e => Err(e),
            }));
        }
    });
    Ok(())
}

pub async fn core_enable_vc(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreVectorCatchRequest,
) -> NoResponse {
    let cond = convert::from_wire_vector_catch_condition(request.condition);
    with_core!(ctx, request.sessid, request.core, |core| {
        probe_rs_try!(core.enable_vector_catch(cond));
    });
    Ok(())
}

pub async fn core_metadata(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreAccessRequest,
) -> RpcResult<WireCoreMetadata> {
    let metadata = with_core!(ctx, request.sessid, request.core, |core| {
        let fpu_support = probe_rs_try!(core.fpu_support());
        let floating_point_register_count = probe_rs_try!(
            fpu_support
                .then(|| core.floating_point_register_count())
                .transpose()
        )
        .map(|count| count as u64);

        let instruction_set =
            convert::to_wire_instruction_set(probe_rs_try!(core.instruction_set()));

        WireCoreMetadata {
            fpu_support,
            floating_point_register_count,
            instruction_set,
        }
    });
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
    let ids: Vec<RegisterId> = request
        .ids
        .iter()
        .copied()
        .map(convert::from_wire_register_id)
        .collect();
    let values = with_core!(ctx, request.sessid, request.core, |core| {
        let mut out: Vec<Result<RegisterValue, RpcError>> = Vec::with_capacity(ids.len());
        for id in &ids {
            out.push(
                core.read_core_reg::<RegisterValue>(*id)
                    .map_err(crate::rpc::functions::convert::rpc_error_probe_rs),
            );
        }
        out
    });

    Ok(request
        .ids
        .into_iter()
        .zip(values)
        .map(|(id, result)| WireRegisterReadResult {
            id,
            result: result.map(convert::to_wire_register_value),
        })
        .collect())
}

pub async fn core_dump(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoreDumpRequest,
) -> RpcResult<WireCoreDump> {
    let mut session = ctx.session(request.sessid).await;
    let mut core = lift(session.core(request.core as usize))?;

    let dump = lift(CoreDump::dump_core(&mut core, request.ranges))?;

    Ok(WireCoreDump {
        registers: dump
            .registers
            .into_iter()
            .map(|(id, v)| {
                (
                    convert::to_wire_register_id(id),
                    convert::to_wire_register_value(v),
                )
            })
            .collect(),
        data: dump.data,
        instruction_set: convert::to_wire_instruction_set(dump.instruction_set),
        supports_native_64bit_access: dump.supports_native_64bit_access,
        core_type: convert::to_wire_core_type(dump.core_type),
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
    let mut core = lift(session.core(request.core as usize))?;

    let status = lift(core.status())?;
    let command = match status {
        CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(c))) => Some(c),
        _ => None,
    };
    let Some(command) = command else {
        return Ok(HandleSemihostingResult {
            status: convert::to_wire_core_status(status),
            events: vec![],
        });
    };

    let Some(state) = guard.get_mut(&request.sessid) else {
        Err("No debug state for session")?
    };
    let sh = state.semihosting_state(request.core as usize);

    let mut events = Vec::new();
    let result = lift(handle_semihosting_impl(&mut core, sh, command, &mut events))?;
    Ok(HandleSemihostingResult {
        status: convert::to_wire_core_status(result),
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

    pub(crate) fn to_wire_register_id(value: RegisterId) -> WireRegisterId {
        WireRegisterId(value.0)
    }

    pub(crate) fn from_wire_register_id(value: WireRegisterId) -> RegisterId {
        RegisterId(value.0)
    }

    pub(crate) fn to_wire_register_value(value: RegisterValue) -> WireRegisterValue {
        match value {
            RegisterValue::U32(v) => WireRegisterValue::U32(v),
            RegisterValue::U64(v) => WireRegisterValue::U64(v),
            RegisterValue::U128(v) => WireRegisterValue::U128(v),
        }
    }

    pub(crate) fn from_wire_register_value(value: WireRegisterValue) -> RegisterValue {
        match value {
            WireRegisterValue::U32(v) => RegisterValue::U32(v),
            WireRegisterValue::U64(v) => RegisterValue::U64(v),
            WireRegisterValue::U128(v) => RegisterValue::U128(v),
        }
    }

    pub(crate) fn to_wire_core_information(value: CoreInformation) -> WireCoreInformation {
        WireCoreInformation { pc: value.pc }
    }

    pub(crate) fn from_wire_core_information(value: WireCoreInformation) -> CoreInformation {
        CoreInformation { pc: value.pc }
    }

    pub(crate) fn to_wire_exit_error_details(value: &ExitErrorDetails) -> WireExitErrorDetails {
        WireExitErrorDetails {
            reason: value.reason,
            exit_status: value.exit_status,
            subcode: value.subcode,
        }
    }

    pub(crate) fn from_wire_exit_error_details(value: WireExitErrorDetails) -> ExitErrorDetails {
        ExitErrorDetails {
            reason: value.reason,
            exit_status: value.exit_status,
            subcode: value.subcode,
        }
    }

    pub(crate) fn to_wire_semihosting_command(
        value: &SemihostingCommand,
    ) -> WireSemihostingCommand {
        match value {
            SemihostingCommand::ExitSuccess => WireSemihostingCommand::ExitSuccess,
            SemihostingCommand::ExitError(details) => {
                WireSemihostingCommand::ExitError(to_wire_exit_error_details(details))
            }
            SemihostingCommand::GetCommandLine(request) => WireSemihostingCommand::GetCommandLine {
                block_address: request.block_address(),
            },
            _ => WireSemihostingCommand::Other,
        }
    }

    pub(crate) fn to_wire_core_status(value: CoreStatus) -> WireCoreStatus {
        match value {
            CoreStatus::Running => WireCoreStatus::Running,
            CoreStatus::Halted(reason) => WireCoreStatus::Halted(to_wire_halt_reason(reason)),
            CoreStatus::LockedUp => WireCoreStatus::LockedUp,
            CoreStatus::Sleeping => WireCoreStatus::Sleeping,
            CoreStatus::Unknown => WireCoreStatus::Unknown,
        }
    }

    pub(crate) fn to_wire_halt_reason(value: HaltReason) -> WireHaltReason {
        use probe_rs::BreakpointCause;
        match value {
            HaltReason::Multiple => WireHaltReason::Multiple,
            HaltReason::Breakpoint(cause) => WireHaltReason::Breakpoint(match cause {
                BreakpointCause::Hardware => WireBreakpointCause::Hardware,
                BreakpointCause::Software => WireBreakpointCause::Software,
                BreakpointCause::Unknown => WireBreakpointCause::Unknown,
                BreakpointCause::Semihosting(ref cmd) => {
                    WireBreakpointCause::Semihosting(to_wire_semihosting_command(cmd))
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

    pub(crate) fn from_wire_breakpoint_cause(
        value: WireBreakpointCause,
    ) -> probe_rs::BreakpointCause {
        match value {
            WireBreakpointCause::Hardware => probe_rs::BreakpointCause::Hardware,
            WireBreakpointCause::Software => probe_rs::BreakpointCause::Software,
            WireBreakpointCause::Unknown => probe_rs::BreakpointCause::Unknown,
            WireBreakpointCause::Semihosting(cmd) => {
                probe_rs::BreakpointCause::Semihosting(match cmd {
                    WireSemihostingCommand::ExitSuccess => SemihostingCommand::ExitSuccess,
                    WireSemihostingCommand::ExitError(details) => {
                        SemihostingCommand::ExitError(from_wire_exit_error_details(details))
                    }
                    WireSemihostingCommand::GetCommandLine { .. } => {
                        SemihostingCommand::Unknown(UnknownCommandDetails {
                            operation: 0,
                            parameter: 0,
                        })
                    }
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
    pub(crate) fn from_wire_halt_reason(value: WireHaltReason) -> HaltReason {
        match value {
            WireHaltReason::Multiple => HaltReason::Multiple,
            WireHaltReason::Breakpoint(cause) => {
                HaltReason::Breakpoint(from_wire_breakpoint_cause(cause))
            }
            WireHaltReason::Exception => HaltReason::Exception,
            WireHaltReason::Watchpoint => HaltReason::Watchpoint,
            WireHaltReason::Step => HaltReason::Step,
            WireHaltReason::Request => HaltReason::Request,
            WireHaltReason::External => HaltReason::External,
            WireHaltReason::Unknown => HaltReason::Unknown,
        }
    }

    pub(crate) fn from_wire_core_status(value: WireCoreStatus) -> CoreStatus {
        match value {
            WireCoreStatus::Running => CoreStatus::Running,
            WireCoreStatus::Halted(reason) => CoreStatus::Halted(from_wire_halt_reason(reason)),
            WireCoreStatus::LockedUp => CoreStatus::LockedUp,
            WireCoreStatus::Sleeping => CoreStatus::Sleeping,
            WireCoreStatus::Unknown => CoreStatus::Unknown,
        }
    }

    pub(crate) fn to_wire_vector_catch_condition(
        value: VectorCatchCondition,
    ) -> WireVectorCatchCondition {
        match value {
            VectorCatchCondition::HardFault => WireVectorCatchCondition::HardFault,
            VectorCatchCondition::CoreReset => WireVectorCatchCondition::CoreReset,
            VectorCatchCondition::SecureFault => WireVectorCatchCondition::SecureFault,
            VectorCatchCondition::All => WireVectorCatchCondition::All,
            VectorCatchCondition::Svc => WireVectorCatchCondition::Svc,
            VectorCatchCondition::Hlt => WireVectorCatchCondition::Hlt,
        }
    }

    pub(crate) fn from_wire_vector_catch_condition(
        value: WireVectorCatchCondition,
    ) -> VectorCatchCondition {
        match value {
            WireVectorCatchCondition::HardFault => VectorCatchCondition::HardFault,
            WireVectorCatchCondition::CoreReset => VectorCatchCondition::CoreReset,
            WireVectorCatchCondition::SecureFault => VectorCatchCondition::SecureFault,
            WireVectorCatchCondition::All => VectorCatchCondition::All,
            WireVectorCatchCondition::Svc => VectorCatchCondition::Svc,
            WireVectorCatchCondition::Hlt => VectorCatchCondition::Hlt,
        }
    }

    pub(crate) fn to_wire_instruction_set(value: InstructionSet) -> WireInstructionSet {
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

    pub(crate) fn from_wire_instruction_set(value: WireInstructionSet) -> InstructionSet {
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

    pub(crate) fn from_wire_stepping_mode(mode: WireSteppingMode) -> SteppingMode {
        match mode {
            WireSteppingMode::StepInstruction => SteppingMode::StepInstruction,
            WireSteppingMode::OverStatement => SteppingMode::OverStatement,
            WireSteppingMode::IntoStatement => SteppingMode::IntoStatement,
            WireSteppingMode::OutOfStatement => SteppingMode::OutOfStatement,
        }
    }

    pub(crate) fn to_wire_core_type(value: probe_rs::CoreType) -> WireCoreType {
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

    pub(crate) fn from_wire_core_type(value: WireCoreType) -> probe_rs::CoreType {
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
