use crate::rpc::{
    Key,
    functions::{NoResponse, RpcContext, RpcResult},
};
use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::Session;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Schema)]
pub struct LockDeviceRequest {
    pub sessid: Key<Session>,
    pub level: Option<String>,
}

pub async fn lock_device(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: LockDeviceRequest,
) -> NoResponse {
    let mut session = ctx.session(request.sessid).await;
    session.lock_device(request.level.as_deref())?;
    Ok(())
}

#[derive(Serialize, Deserialize, Schema)]
pub struct SupportedLockLevelsRequest {
    pub sessid: Key<Session>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct LockLevelInfo {
    pub name: String,
    pub description: String,
    pub is_permanent: bool,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct SupportedLockLevelsData {
    pub levels: Vec<LockLevelInfo>,
}

pub type SupportedLockLevelsResponse = RpcResult<SupportedLockLevelsData>;

pub async fn supported_lock_levels(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: SupportedLockLevelsRequest,
) -> SupportedLockLevelsResponse {
    let session = ctx.session(request.sessid).await;
    let levels = session.supported_lock_levels()?;
    Ok(SupportedLockLevelsData {
        levels: levels
            .into_iter()
            .map(|l| LockLevelInfo {
                name: l.name,
                description: l.description,
                is_permanent: l.is_permanent,
            })
            .collect(),
    })
}
