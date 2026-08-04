use postcard_rpc::header::VarHeader;
use probe_rs_rpc::resume::ResumeAllCoresRequest;

use crate::rpc::functions::{RpcContext, convert::lift};
use probe_rs_rpc::NoResponse;

pub async fn resume_all_cores(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ResumeAllCoresRequest,
) -> NoResponse {
    lift(ctx.session(request.sessid).await.resume_all_cores())?;
    Ok(())
}
