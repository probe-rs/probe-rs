use std::time::Duration;

use crate::rpc::{
    Key,
    functions::{NoResponse, RpcContext},
};
use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::Session;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Schema)]
pub struct ResetCoreRequest {
    pub sessid: Key<Session>,
    pub core: u32,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct ResetCoreAndHaltRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub timeout: Duration,
}

pub async fn reset(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ResetCoreRequest,
) -> NoResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut core = session.core(request.core as usize)?;
    core.reset()?;
    Ok(())
}

pub async fn reset_and_halt(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ResetCoreAndHaltRequest,
) -> NoResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut core = session.core(request.core as usize)?;
    core.reset_and_halt(request.timeout)?;
    Ok(())
}
