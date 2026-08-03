#[cfg(feature = "remote")]
use anyhow::Context as _;
use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::rpc::{
    Key, TempFileHandle,
    functions::{NoResponse, RpcContext, RpcResult},
};

#[cfg(feature = "remote")]
use tempfile::NamedTempFile;

#[derive(Serialize, Deserialize, Schema)]
pub struct TempFile {
    pub path: String,
    pub key: Key<TempFileHandle>,
}

pub type CreateFileResponse = RpcResult<TempFile>;

#[derive(Serialize, Deserialize, Schema)]
pub struct AppendFileRequest {
    pub data: Vec<u8>,
    pub key: Key<TempFileHandle>,
}

#[cfg(feature = "remote")]
pub async fn create_temp_file(
    ctx: &mut RpcContext,
    _header: VarHeader,
    _req: (),
) -> CreateFileResponse {
    // TODO: avoid temp files altogether
    let file = NamedTempFile::new().context("Failed to write temporary file")?;
    let path = file.path().to_path_buf().display().to_string();
    tracing::info!("Created temporary file {}", path);
    let key = ctx.store_object(file).await;

    Ok(TempFile { path, key })
}

#[cfg(not(feature = "remote"))]
pub async fn create_temp_file(
    _ctx: &mut RpcContext,
    _header: VarHeader,
    _req: (),
) -> CreateFileResponse {
    Err("Not supported".into())
}

#[cfg(feature = "remote")]
pub async fn append_temp_file(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: AppendFileRequest,
) -> NoResponse {
    use std::io::Write as _;

    let mut file = ctx.object_mut(request.key).await;

    file.as_file_mut()
        .write_all(&request.data)
        .context("Failed to write temporary file")?;

    Ok(())
}

#[cfg(not(feature = "remote"))]
pub async fn append_temp_file(
    _ctx: &mut RpcContext,
    _header: VarHeader,
    _request: AppendFileRequest,
) -> NoResponse {
    Err("Not supported".into())
}
