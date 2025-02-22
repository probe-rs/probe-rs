use crate::rpc::{
    Key,
    functions::{NoResponse, RpcContext},
};
use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::Session;
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
    ctx.session(request.sessid).await.resume_all_cores()?;
    Ok(())
}
