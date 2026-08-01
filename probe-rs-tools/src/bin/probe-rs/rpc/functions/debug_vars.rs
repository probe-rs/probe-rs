use postcard_rpc::header::VarHeader;
use probe_rs_debug::{
    DebugInfo, DebugRegisters, ObjectRef, StackFrameInfo, Variable, VariableCache, VariableName,
};
use probe_rs_rpc::debug_vars::{
    ClearCoreDebugStateRequest, EvaluateRequest, EvaluateResponse, LoadSvdRequest, LoadSvdResponse,
    ScopesRequest, ScopesResponse, SetVariableRequest, SetVariableResult, VariablesRequest,
    VariablesResponse, WireEvaluateResponse, WireScope, WireSetVariableResponse, WireVariable,
};

use crate::rpc::functions::{RpcContext, convert::lift};
use probe_rs_rpc::RpcResult;

/// Mirrors `request_helpers::get_variable_reference` for the server-side path.
fn variable_reference(parent: &Variable, cache: &VariableCache) -> (ObjectRef, i64, i64) {
    if !parent.is_valid() {
        return (ObjectRef::Invalid, 0, 0);
    }
    let mut named = 0;
    let mut indexed = 0;
    for child in cache.get_children(parent.variable_key()) {
        if child.is_indexed() {
            indexed += 1;
        } else {
            named += 1;
        }
    }
    if named > 0 || indexed > 0 {
        (parent.variable_key(), named, indexed)
    } else if parent.variable_node_type.is_deferred() && parent.to_string(cache) != "()" {
        (parent.variable_key(), 0, 0)
    } else {
        (ObjectRef::Invalid, 0, 0)
    }
}

pub async fn scopes(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ScopesRequest,
) -> ScopesResponse {
    Ok(ctx
        .with_core_debug_state_mut(request.sessid, request.core, |core_state| {
            let frame_ref = ObjectRef::from(request.frame_id as i64);
            let mut scopes: Vec<WireScope> = Vec::new();

            if let Some(static_cache) = &core_state.static_variables {
                scopes.push(WireScope {
                    name: "Static".to_string(),
                    presentation_hint: Some("statics".to_string()),
                    variables_reference: i64::from(static_cache.root_variable().variable_key()),
                    expensive: true,
                    line: None,
                    column: None,
                });
            }

            if let Some(svd_cache) = &core_state.svd_variables {
                scopes.push(WireScope {
                    name: "Peripherals".to_string(),
                    presentation_hint: None,
                    variables_reference: i64::from(svd_cache.root_variable_key()),
                    expensive: true,
                    line: None,
                    column: None,
                });
            }

            if let Some(frame) = core_state.stack_frames.iter().find(|f| f.id == frame_ref) {
                // Registers scope: reuse the frame id as its variables_reference.
                scopes.push(WireScope {
                    name: "Registers".to_string(),
                    presentation_hint: Some("registers".to_string()),
                    variables_reference: i64::from(frame.id),
                    expensive: true,
                    line: None,
                    column: None,
                });

                if let Some(locals) = &frame.local_variables {
                    let line = frame
                        .source_location
                        .as_ref()
                        .and_then(|l| l.line.map(|l| l as i64));
                    let column = frame.source_location.as_ref().and_then(|l| {
                        l.column.map(|c| match c {
                            probe_rs_debug::ColumnType::LeftEdge => 0,
                            probe_rs_debug::ColumnType::Column(c) => c as i64,
                        })
                    });
                    scopes.push(WireScope {
                        name: "Variables".to_string(),
                        presentation_hint: Some("locals".to_string()),
                        variables_reference: i64::from(locals.root_variable().variable_key()),
                        expensive: false,
                        line,
                        column,
                    });
                }
            }

            scopes
        })
        .await?)
}

pub async fn variables(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: VariablesRequest,
) -> VariablesResponse {
    let states = ctx.debug_states();
    let mut guard = states.lock().await;
    let debug_info = guard
        .get(&request.sessid)
        .and_then(|s| s.debug_info.clone());
    let Some(debug_info) = debug_info else {
        Err("No debug state for session")?
    };

    // Take the session/core AFTER locking debug_states: `probe_rs::Core` is
    // `!Send`, so it must not be held across the `debug_states().lock().await`
    // (the spawned server future must remain `Send`).
    let mut session = ctx.session(request.sessid).await;
    let mut core = lift(session.core(request.core as usize))?;

    let Some(state) = guard.get_mut(&request.sessid) else {
        Err("No debug state for session")?
    };
    let Some(core_state) = state.per_core.get_mut(&(request.core as usize)) else {
        Err("No debug state for core")?
    };

    let variable_ref = ObjectRef::from(request.variables_reference as i64);

    let mut parent_variable: Option<Variable> = None;
    let mut variable_cache: Option<&mut VariableCache> = None;
    let mut frame_info: Option<StackFrameInfo<'_>> = None;
    let cloned_registers = core_state.stack_frames.first().map(|f| f.registers.clone());
    if let Some(frame) = core_state
        .stack_frames
        .iter()
        .find(|f| f.id == variable_ref)
    {
        let regs: Vec<WireVariable> = frame
            .registers
            .0
            .iter()
            .map(|register| WireVariable {
                name: register.get_register_name(),
                evaluate_name: Some(register.get_register_name()),
                memory_reference: None,
                indexed_variables: None,
                named_variables: None,
                type_: Some(format!("{}", VariableName::RegistersRoot)),
                value: register.value.unwrap_or_default().to_string(),
                variables_reference: 0,
            })
            .collect();
        return Ok(regs);
    }

    if let Some(svd_cache) = core_state.svd_variables.as_ref()
        && svd_cache.get_variable_by_key(variable_ref).is_some()
    {
        let dap_variables: Vec<WireVariable> = svd_cache
            .get_children(variable_ref)
            .into_iter()
            .map(|variable| {
                let child_count = svd_cache.get_children(variable.variable_key()).len() as i64;
                WireVariable {
                    name: variable.name().to_string(),
                    evaluate_name: None,
                    memory_reference: variable.memory_reference(),
                    indexed_variables: Some(0),
                    named_variables: Some(child_count),
                    type_: variable.type_name(),
                    value: variable.get_value(&mut core),
                    variables_reference: i64::from(variable.variable_key()),
                }
            })
            .collect();
        return Ok(dap_variables);
    }

    if let Some(search_cache) = core_state.static_variables.as_mut()
        && let Some(search_variable) = search_cache.get_variable_by_key(variable_ref)
    {
        parent_variable = Some(search_variable);
        variable_cache = Some(search_cache);
        if let Some(regs) = &cloned_registers {
            let top_frame = core_state.stack_frames.first().unwrap();
            frame_info = Some(StackFrameInfo {
                registers: regs,
                frame_base: top_frame.frame_base,
                canonical_frame_address: top_frame.canonical_frame_address,
            });
        }
    }

    if parent_variable.is_none() {
        for frame in core_state.stack_frames.iter_mut() {
            if let Some(search_cache) = frame.local_variables.as_mut()
                && let Some(search_variable) = search_cache.get_variable_by_key(variable_ref)
            {
                parent_variable = Some(search_variable);
                variable_cache = Some(search_cache);
                frame_info = Some(StackFrameInfo {
                    registers: &frame.registers,
                    frame_base: frame.frame_base,
                    canonical_frame_address: frame.canonical_frame_address,
                });
                break;
            }
        }
    }

    let Some(variable_cache) = variable_cache else {
        Err(format!(
            "No variable information found for {}!",
            request.variables_reference
        ))?
    };

    if let Some(parent) = parent_variable.as_mut()
        && parent.variable_node_type.is_deferred()
        && !variable_cache.has_children(parent)
        && let Some(frame_info) = frame_info
    {
        lift(debug_info.cache_deferred_variables(variable_cache, &mut core, parent, frame_info))?;
    }

    Ok(variable_cache
        .get_children(variable_ref)
        .filter(|variable| match &request.filter {
            Some(filter) => match filter.as_str() {
                "indexed" => variable.is_indexed(),
                "named" => !variable.is_indexed(),
                _ => false,
            },
            None => true,
        })
        .map(|variable| {
            let (variables_reference, named_cnt, indexed_cnt) =
                variable_reference(variable, variable_cache);
            WireVariable {
                name: variable.name.to_string(),
                evaluate_name: None,
                memory_reference: Some(variable.memory_location.to_string()),
                indexed_variables: Some(indexed_cnt),
                named_variables: Some(named_cnt),
                type_: Some(variable.type_name()),
                value: variable.to_string(variable_cache),
                variables_reference: i64::from(variables_reference),
            }
        })
        .collect())
}

pub async fn clear_core_debug_state(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ClearCoreDebugStateRequest,
) -> RpcResult<()> {
    let states = ctx.debug_states();
    let mut guard = states.lock().await;
    if let Some(state) = guard.get_mut(&request.sessid) {
        state.clear_core(request.core as usize);
    }
    Ok(())
}

/// Replace the per-core SVD state from `request.path`, or clear it when no
/// path is supplied. A parse failure also clears the previous cache so stale
/// peripheral metadata is never retained after a requested reload.
pub async fn load_svd(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: LoadSvdRequest,
) -> LoadSvdResponse {
    let parsed = request
        .path
        .as_deref()
        .map(|path| crate::rpc::svd::parse_svd_file(std::path::Path::new(path)))
        .transpose();

    ctx.with_server_debug_state_mut(request.sessid, |state| match parsed {
        Ok(svd_variables) => {
            state.replace_svd(request.core as usize, svd_variables);
            Ok(())
        }
        Err(error) => {
            state.replace_svd(request.core as usize, None);
            Err(crate::rpc::functions::convert::rpc_error_debug(error))
        }
    })
    .await
}

/// Set a local/static variable's value server-side. The client only relays
/// the parent key, name, and new value. Searches per-frame locals then
/// statics by name+parent, runs `Variable::update_value` against the live
/// `Core`, and returns the response fields for the DAP `setVariable` body.
pub async fn set_variable(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: SetVariableRequest,
) -> SetVariableResult {
    let states = ctx.debug_states();
    let mut guard = states.lock().await;

    let mut session = ctx.session(request.sessid).await;
    let mut core = lift(session.core(request.core as usize))?;

    let Some(state) = guard.get_mut(&request.sessid) else {
        Err("No debug state for session")?
    };
    let Some(core_state) = state.per_core.get_mut(&(request.core as usize)) else {
        Err("No debug state for core")?
    };

    let parent_key = ObjectRef::from(request.parent_key);
    let variable_name = VariableName::Named(request.name.clone());

    let mut cache_variable: Option<Variable> = None;
    let mut variable_cache: Option<&mut VariableCache> = None;
    for frame in core_state.stack_frames.iter_mut() {
        if let Some(search_cache) = frame.local_variables.as_mut()
            && let Some(search_variable) =
                search_cache.get_variable_by_name_and_parent(&variable_name, parent_key)
        {
            cache_variable = Some(search_variable);
            variable_cache = Some(search_cache);
            break;
        }
    }
    if cache_variable.is_none()
        && let Some(search_cache) = core_state.static_variables.as_mut()
        && let Some(search_variable) =
            search_cache.get_variable_by_name_and_parent(&variable_name, parent_key)
    {
        cache_variable = Some(search_variable);
        variable_cache = Some(search_cache);
    }

    let (Some(cache_variable), Some(variable_cache)) = (cache_variable, variable_cache) else {
        Err(format!(
            "No variable information found for {variable_name}!"
        ))?
    };

    cache_variable
        .update_value(&mut core, variable_cache, request.value.clone())
        .map_err(|e| e.to_string())?;
    let (vr, named, indexed) = variable_reference(&cache_variable, variable_cache);
    Ok(WireSetVariableResponse {
        value: request.value,
        type_: Some(format!("{:?}", cache_variable.type_name())),
        variables_reference: i64::from(vr),
        named_variables: Some(named),
        indexed_variables: Some(indexed),
        memory_reference: Some(cache_variable.memory_location.to_string()),
    })
}

/// Resolve a DAP evaluate expression against a `VariableCache`, expanding the
/// single-root deferred case first. Returns `None` if the expression names no
/// variable in `cache`.
fn resolve_expression(
    debug_info: &DebugInfo,
    core: &mut probe_rs::Core,
    cache: &mut VariableCache,
    expression: &str,
    frame_info: StackFrameInfo<'_>,
) -> Option<WireEvaluateResponse> {
    if cache.len() == 1 {
        let mut root = cache.root_variable().clone();
        if root.variable_node_type.is_deferred() && !cache.has_children(&root) {
            debug_info
                .cache_deferred_variables(cache, core, &mut root, frame_info)
                .ok()?;
        }
    }
    let mut variable = if let Ok(key) = expression.parse::<ObjectRef>() {
        cache.get_variable_by_key(key)
    } else {
        cache.get_variable_by_name(&VariableName::Named(expression.to_string()))
    }?;
    let (vr, named, indexed) = variable_reference(&variable, cache);
    variable.extract_value(core, cache);
    cache.update_variable(&variable).ok()?;
    Some(WireEvaluateResponse {
        result: variable.to_string(cache),
        type_: Some(variable.type_name()),
        variables_reference: i64::from(vr),
        named_variables: Some(named),
        indexed_variables: Some(indexed),
        memory_reference: Some(variable.memory_location.to_string()),
    })
}

pub async fn evaluate(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: EvaluateRequest,
) -> EvaluateResponse {
    let states = ctx.debug_states();
    let mut guard = states.lock().await;
    let debug_info = guard
        .get(&request.sessid)
        .and_then(|s| s.debug_info.clone());
    let Some(debug_info) = debug_info else {
        Err("No debug state for session")?
    };

    let mut session = ctx.session(request.sessid).await;
    let mut core = lift(session.core(request.core as usize))?;

    let Some(state) = guard.get_mut(&request.sessid) else {
        Err("No debug state for session")?
    };
    let Some(core_state) = state.per_core.get_mut(&(request.core as usize)) else {
        Err("No debug state for core")?
    };

    let invalid = || WireEvaluateResponse {
        result: format!("<invalid expression {:?}>", request.expression),
        type_: None,
        variables_reference: 0,
        named_variables: None,
        indexed_variables: None,
        memory_reference: None,
    };

    let frame_ref = match request.frame_id {
        Some(id) => ObjectRef::from(id as i64),
        None => core_state
            .stack_frames
            .first()
            .map(|f| f.id)
            .unwrap_or(ObjectRef::Invalid),
    };
    if matches!(frame_ref, ObjectRef::Invalid) {
        return Ok(invalid());
    }
    let Some(frame_index) = core_state
        .stack_frames
        .iter()
        .position(|f| f.id == frame_ref)
    else {
        return Ok(invalid());
    };

    if let Some(reg) = core_state.stack_frames[frame_index]
        .registers
        .get_register_by_name(&request.expression)
        .and_then(|r| r.value)
    {
        return Ok(WireEvaluateResponse {
            result: format!("{reg}"),
            type_: Some(format!("{}", VariableName::RegistersRoot)),
            variables_reference: 0,
            named_variables: None,
            indexed_variables: None,
            memory_reference: None,
        });
    }

    let frame_base = core_state.stack_frames[frame_index].frame_base;
    let cfa = core_state.stack_frames[frame_index].canonical_frame_address;
    let frame_regs = core_state.stack_frames[frame_index].registers.clone();

    if let Some(cache) = core_state.stack_frames[frame_index]
        .local_variables
        .as_mut()
        && let Some(resp) = resolve_expression(
            &debug_info,
            &mut core,
            cache,
            &request.expression,
            StackFrameInfo {
                registers: &frame_regs,
                frame_base,
                canonical_frame_address: cfa,
            },
        )
    {
        return Ok(resp);
    }

    if let Some(cache) = core_state.static_variables.as_mut()
        && let Some(resp) = {
            let (top_base, top_cfa, top_regs) = core_state
                .stack_frames
                .first()
                .map(|f| (f.frame_base, f.canonical_frame_address, f.registers.clone()))
                .unwrap_or((None, None, DebugRegisters::default()));
            resolve_expression(
                &debug_info,
                &mut core,
                cache,
                &request.expression,
                StackFrameInfo {
                    registers: &top_regs,
                    frame_base: top_base,
                    canonical_frame_address: top_cfa,
                },
            )
        }
    {
        return Ok(resp);
    }

    Ok(invalid())
}
