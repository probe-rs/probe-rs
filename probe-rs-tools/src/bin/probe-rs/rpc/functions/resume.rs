use crate::rpc::{
    Key, Session,
    functions::{NoResponse, RpcContext, convert::lift},
};
use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Schema)]
pub struct ResumeAllCoresRequest {
    pub sessid: Key<Session>,
}

pub async fn resume_all_cores(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ResumeAllCoresRequest,
) -> NoResponse {
    lift(ctx.session(request.sessid).await.resume_all_cores())?;
    Ok(())
}
