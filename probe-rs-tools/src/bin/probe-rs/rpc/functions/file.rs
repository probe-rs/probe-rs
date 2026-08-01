#[cfg(feature = "remote")]
use anyhow::Context as _;
use postcard_rpc::header::VarHeader;
use probe_rs_rpc::file::{AppendFileRequest, CreateFileResponse};

use crate::rpc::functions::RpcContext;
use probe_rs_rpc::NoResponse;

#[cfg(feature = "remote")]
use crate::rpc::functions::convert::lift;

#[cfg(feature = "remote")]
use tempfile::NamedTempFile;

#[cfg(feature = "remote")]
pub async fn create_temp_file(
    ctx: &mut RpcContext,
    _header: VarHeader,
    _req: (),
) -> CreateFileResponse {
    // TODO: avoid temp files altogether
    let file = lift(NamedTempFile::new().context("Failed to write temporary file"))?;
    let path = file.path().to_path_buf().display().to_string();
    tracing::info!("Created temporary file {}", path);
    let key = ctx.store_object(file).await;

    Ok(probe_rs_rpc::file::TempFile { path, key })
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

    lift(
        file.as_file_mut()
            .write_all(&request.data)
            .context("Failed to write temporary file"),
    )?;

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
