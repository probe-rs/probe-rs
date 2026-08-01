use postcard_rpc::header::VarHeader;
use probe_rs_rpc::reset::{ResetCoreAndHaltRequest, ResetCoreRequest};

use crate::rpc::functions::{RpcContext, convert::lift};
use probe_rs_rpc::core_ops::WireCoreInformation;
use probe_rs_rpc::{NoResponse, RpcResult};

pub async fn reset(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ResetCoreRequest,
) -> NoResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut core = lift(session.core(request.core as usize))?;
    lift(core.reset())?;
    Ok(())
}

pub async fn reset_and_halt(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ResetCoreAndHaltRequest,
) -> RpcResult<WireCoreInformation> {
    let mut session = ctx.session(request.sessid).await;
    let mut core = lift(session.core(request.core as usize))?;
    let info = lift(core.reset_and_halt(request.timeout))?;
    Ok(crate::rpc::functions::core_ops::convert::to_wire_core_information(info))
}
