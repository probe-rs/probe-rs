use std::time::Duration;

use postcard_rpc::header::VarHeader;
use probe_rs::Error;
use probe_rs_rpc::cores::{CoresRequest, CoresStatusMap, CoresStatusResponse, HaltCoresRequest};

use crate::rpc::functions::RpcContext;
use crate::rpc::functions::core_ops::convert::to_wire_core_status;

pub async fn halt_cores(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: HaltCoresRequest,
) -> CoresStatusResponse {
    let mut session = ctx.session(request.sessid).await;
    let core_indices = resolve_core_indices(&session, request.cores.as_deref())?;
    let mut statuses = Vec::with_capacity(core_indices.len());

    for core_index in core_indices {
        match operate_halt(&mut session, core_index, request.timeout) {
            Ok(status) => statuses.push((core_index as u32, to_wire_core_status(status))),
            Err(Error::CoreDisabled(_)) => {}
            Err(error) => return Err(crate::rpc::functions::convert::rpc_error_probe_rs(error)),
        }
    }

    Ok(CoresStatusMap { statuses })
}

pub async fn resume_cores(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoresRequest,
) -> CoresStatusResponse {
    let mut session = ctx.session(request.sessid).await;
    let core_indices = resolve_core_indices(&session, request.cores.as_deref())?;
    let mut statuses = Vec::with_capacity(core_indices.len());

    for core_index in core_indices {
        match operate_resume(&mut session, core_index) {
            Ok(status) => statuses.push((core_index as u32, to_wire_core_status(status))),
            Err(Error::CoreDisabled(_)) => {}
            Err(error) => return Err(crate::rpc::functions::convert::rpc_error_probe_rs(error)),
        }
    }

    Ok(CoresStatusMap { statuses })
}

pub async fn cores_status(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CoresRequest,
) -> CoresStatusResponse {
    let mut session = ctx.session(request.sessid).await;
    let core_indices = resolve_core_indices(&session, request.cores.as_deref())?;
    let mut statuses = Vec::with_capacity(core_indices.len());

    for core_index in core_indices {
        match operate_status(&mut session, core_index) {
            Ok(status) => statuses.push((core_index as u32, to_wire_core_status(status))),
            Err(Error::CoreDisabled(_)) => {}
            Err(error) => return Err(crate::rpc::functions::convert::rpc_error_probe_rs(error)),
        }
    }

    Ok(CoresStatusMap { statuses })
}

fn resolve_core_indices(
    session: &probe_rs::Session,
    cores: Option<&[u32]>,
) -> Result<Vec<usize>, probe_rs_rpc::RpcError> {
    match cores {
        Some(cores) => Ok(cores.iter().map(|core| *core as usize).collect()),
        None => Ok((0..session.list_cores().len()).collect()),
    }
}

fn operate_halt(
    session: &mut probe_rs::Session,
    core_index: usize,
    timeout: Duration,
) -> Result<probe_rs::CoreStatus, Error> {
    let mut core = session.core(core_index)?;
    if !core.core_halted()? {
        core.halt(timeout)?;
    }
    core.status()
}

fn operate_resume(
    session: &mut probe_rs::Session,
    core_index: usize,
) -> Result<probe_rs::CoreStatus, Error> {
    let mut core = session.core(core_index)?;
    if core.core_halted()? {
        core.run()?;
    }
    core.status()
}

fn operate_status(
    session: &mut probe_rs::Session,
    core_index: usize,
) -> Result<probe_rs::CoreStatus, Error> {
    session.core(core_index)?.status()
}
