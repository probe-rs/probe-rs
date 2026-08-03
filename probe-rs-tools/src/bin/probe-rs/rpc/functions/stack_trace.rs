use std::sync::Arc;

use postcard_rpc::header::VarHeader;
use probe_rs::{CoreInterface, Error};
use probe_rs_debug::{
    DebugInfo, DebugRegisters, StackFrame, VariableCache, exception_handler_for_core,
};
use probe_rs_rpc::stack_trace::{
    LoadDebugInfoRequest, LoadDebugInfoResponse, RichStackTrace, RichStackTraceFrame,
    RichStackTraces, SourceLocation, StackTrace, StackTraceFrame, StackTraces,
    TakeRichStackTraceRequest, TakeRichStackTraceResponse, TakeStackTraceRequest,
    TakeStackTraceResponse, WireDebugRegister,
};

use crate::rpc::functions::{RpcContext, convert::lift};

/// Eagerly load and cache the authoritative server-side [`DebugInfo`] for a
/// session, keyed by `sessid`, so consumers can resolve source locations
/// before the first halt.
///
/// A subsequent call replaces the cached DWARF and invalidates stack and
/// variable state derived from the previous binary. Parsing completes before
/// the existing state is changed, so a failed reload leaves it intact.
pub async fn load_debug_info(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: LoadDebugInfoRequest,
) -> LoadDebugInfoResponse {
    let debug_info = DebugInfo::from_file(&request.path).map_err(|e| e.to_string())?;
    ctx.with_server_debug_state_mut(request.sessid, |state| {
        state.replace_debug_info(debug_info);
    })
    .await;
    Ok(())
}

/// Shared per-core unwind loop used by [`take_stack_trace`]. Returns
/// `(core_index, frames)` pairs, where each `StackFrame` is converted to `F`
/// via `F::from`.
async fn unwind_all_cores(
    ctx: &mut RpcContext,
    request: &TakeStackTraceRequest,
) -> probe_rs_rpc::RpcResult<Vec<(u32, Vec<StackTraceFrame>)>> {
    let mut session = ctx.session(request.sessid).await;

    let Some(debug_info) = DebugInfo::from_file(&request.path).ok() else {
        Err("No debug info found.")?
    };

    lift(session.halted_access(|session| {
        let mut cores = Vec::new();
        for (idx, core_type) in session.list_cores() {
            let mut core = match session.core(idx) {
                Ok(core) => core,
                Err(Error::CoreDisabled(_)) => continue,
                Err(e) => return Err(e),
            };

            let initial_registers = DebugRegisters::from_core(&mut core);
            let exception_interface = exception_handler_for_core(core_type);
            let instruction_set = core.instruction_set().ok();
            let stack_frames = debug_info.unwind(
                &mut core,
                initial_registers,
                exception_interface.as_ref(),
                instruction_set,
                request.stack_frame_limit as usize,
            )?;

            let frames: Vec<StackTraceFrame> = stack_frames
                .into_iter()
                .map(convert::to_wire_stack_trace_frame)
                .collect();
            cores.push((idx as u32, frames));
        }
        Ok(cores)
    }))
}

pub async fn take_stack_trace(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: TakeStackTraceRequest,
) -> TakeStackTraceResponse {
    let cores = unwind_all_cores(ctx, &request).await?;
    Ok(StackTraces {
        cores: cores
            .into_iter()
            .map(|(core, frames)| StackTrace { core, frames })
            .collect(),
    })
}

/// Like [`take_stack_trace`], but the server owns the per-core
/// `local_variables`/`static_variables` `VariableCache` trees (cached in
/// [`crate::rpc::debug_state::ServerDebugState`], keyed by `sessid`). It
/// returns per-frame register state + metadata plus the server-assigned
/// `id` handles, so an RPC-backed
/// DAP client can resolve `scopes`/`variables` server-side.
///
/// Requires preloaded session debug state from [`load_debug_info`]. Missing
/// state is a deterministic lifecycle error; no path upload or fallback
/// DWARF parsing is performed here.
pub async fn take_rich_stack_trace(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: TakeRichStackTraceRequest,
) -> TakeRichStackTraceResponse {
    let debug_info = ctx
        .with_server_debug_state(request.sessid, |state| state.debug_info.clone())
        .await;

    let mut session = ctx.session(request.sessid).await;

    // Per core: unwind, build locals via `get_stackframe_info`, build the
    // static scope cache.
    let cores: Vec<(
        u32,
        Vec<StackFrame>,
        VariableCache,
        Vec<RichStackTraceFrame>,
    )> = lift(session.halted_access(|session| {
        let mut cores = Vec::new();
        for (idx, core_type) in session.list_cores() {
            if let Some(requested_core) = request.core
                && idx as u32 != requested_core
            {
                continue;
            }

            let mut core = match session.core(idx) {
                Ok(core) => core,
                Err(Error::CoreDisabled(_)) => continue,
                Err(e) => return Err(e),
            };

            // Sessions attached without a program binary have no DWARF. Unwind anyway: the
            // fallback unwinder still walks the call stack, reporting each frame's program
            // counter without a name or source location.
            let debug_info = match &debug_info {
                Some(debug_info) => debug_info.clone(),
                None => Arc::new(DebugInfo::empty(core.endianness()?)),
            };

            let initial_registers = DebugRegisters::from_core(&mut core);
            let exception_interface = exception_handler_for_core(core_type);
            let instruction_set = core.instruction_set().ok();
            let mut stack_frames = debug_info.unwind(
                &mut core,
                initial_registers,
                exception_interface.as_ref(),
                instruction_set,
                request.stack_frame_limit as usize,
            )?;

            let static_variables = debug_info.create_static_scope_cache();

            // Group consecutive frames sharing a register dump (an inlined
            // chain from one `get_stackframe_info` call) and populate
            // `local_variables` for each frame in the group.
            let mut i = 0;
            while i < stack_frames.len() {
                let group_start = i;
                let group_regs = stack_frames[i].registers.clone();
                while i < stack_frames.len() && stack_frames[i].registers == group_regs {
                    i += 1;
                }
                let step_pc: u64 = stack_frames[group_start].pc.try_into().unwrap_or(0);
                let cfa = stack_frames[group_start].canonical_frame_address;
                let mut chain = debug_info
                    .get_stackframe_info(&mut core, step_pc, cfa, &group_regs)
                    .ok()
                    .unwrap_or_default();
                // DIE order is outermost-first; wire order is innermost-first.
                chain.reverse();
                for (offset, frame) in stack_frames[group_start..i].iter_mut().enumerate() {
                    if let Some(cf) = chain.get(offset) {
                        frame.local_variables = cf.local_variables.clone();
                        if frame.source_location.is_none() {
                            frame.source_location = cf.source_location.clone();
                        }
                    }
                }
            }

            let rich_frames = stack_frames
                .iter()
                .map(|f| RichStackTraceFrame {
                    function_name: f.function_name.clone(),
                    program_counter:
                        crate::rpc::functions::core_ops::convert::to_wire_register_value(f.pc),
                    is_inlined: f.is_inlined,
                    location: f
                        .source_location
                        .as_ref()
                        .map(convert::to_wire_source_location),
                    frame_base: f.frame_base,
                    canonical_frame_address: f.canonical_frame_address,
                    registers: f
                        .registers
                        .0
                        .iter()
                        .map(convert::to_wire_debug_register)
                        .collect(),
                    id: i64::from(f.id) as u32,
                })
                .collect();

            cores.push((idx as u32, stack_frames, static_variables, rich_frames));
        }
        Ok(cores)
    }))?;

    drop(session);

    Ok(ctx
        .with_server_debug_state_mut(request.sessid, |state| {
            let wire_cores: Vec<RichStackTrace> = cores
                .into_iter()
                .map(|(core, frames, static_variables, rich_frames)| {
                    state.store_core(core as usize, frames, Some(static_variables));
                    RichStackTrace {
                        core,
                        frames: rich_frames,
                    }
                })
                .collect();
            RichStackTraces { cores: wire_cores }
        })
        .await)
}

pub(crate) mod convert {
    use super::{SourceLocation, StackTraceFrame, WireDebugRegister};
    use crate::rpc::functions::core_ops::convert::{to_wire_register_id, to_wire_register_value};
    use probe_rs_debug::{DebugRegister, StackFrame};

    pub(crate) fn to_wire_source_location(
        location: &probe_rs_debug::SourceLocation,
    ) -> SourceLocation {
        SourceLocation {
            file: location.path.to_path().display().to_string(),
            line: location.line,
            column: location.column.map(|col| match col {
                probe_rs_debug::ColumnType::LeftEdge => 1,
                probe_rs_debug::ColumnType::Column(c) => c,
            }),
        }
    }

    pub(crate) fn to_wire_stack_trace_frame(frame: StackFrame) -> StackTraceFrame {
        to_wire_stack_trace_frame_ref(&frame)
    }

    pub(crate) fn to_wire_stack_trace_frame_ref(frame: &StackFrame) -> StackTraceFrame {
        StackTraceFrame {
            function_name: frame.function_name.clone(),
            program_counter: frame.pc.try_into().unwrap_or(0),
            is_inlined: frame.is_inlined,
            location: frame.source_location.as_ref().map(to_wire_source_location),
        }
    }

    pub(crate) fn to_wire_debug_register(r: &DebugRegister) -> WireDebugRegister {
        WireDebugRegister {
            id: to_wire_register_id(r.core_register.id),
            dwarf_id: r.dwarf_id,
            value: r.value.map(to_wire_register_value),
        }
    }
}
