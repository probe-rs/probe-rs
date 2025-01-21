use std::path::PathBuf;

#[cfg(feature = "remote")]
use anyhow::Context as _;
use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::rpc::{
    functions::{NoResponse, RpcContext, RpcResult},
    Key,
};

#[cfg(feature = "remote")]
use tempfile::NamedTempFile;

#[derive(Serialize, Deserialize, Schema)]
pub struct TempFile {
    pub path: String,
    pub key: Key<PathBuf>,
}

pub type CreateFileResponse = RpcResult<TempFile>;

#[derive(Serialize, Deserialize, Schema)]
pub struct AppendFileRequest {
    pub data: Vec<u8>,
    pub key: Key<PathBuf>,
}

#[cfg(feature = "remote")]
pub fn create_temp_file(ctx: &mut RpcContext, _header: VarHeader, _req: ()) -> CreateFileResponse {
    // TODO: avoid temp files altogether
    let file = NamedTempFile::new().context("Failed to write temporary file")?;
    let path = file.path().to_path_buf().display().to_string();
    tracing::info!("Created temporary file {}", path);
    let key = ctx.store_object_blocking(file);

    Ok(TempFile {
        path,
        key: unsafe { key.cast() },
    })
}

#[cfg(not(feature = "remote"))]
pub fn create_temp_file(_ctx: &mut RpcContext, _header: VarHeader, _req: ()) -> CreateFileResponse {
    Err("Not supported".into())
}

#[cfg(feature = "remote")]
pub async fn append_temp_file(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: AppendFileRequest,
) -> NoResponse {
    use std::io::Write as _;

    let mut file = ctx
        .object_mut::<NamedTempFile>(unsafe { request.key.cast::<NamedTempFile>() })
        .await;

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
