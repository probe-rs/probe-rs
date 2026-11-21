use std::path::Path;

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::{Key, RpcResult, TempFileHandle};

/// A file on the server that the client uploaded.
///
/// The server cannot trust the path that a client sends, so the path is available only through
/// [`ServerPath::checked`]. Only the upload functions of the client create a `ServerPath`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(transparent)]
pub struct ServerPath(String);

impl ServerPath {
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    /// Returns the path, if `access` allows to open the file.
    pub fn checked(&self, access: &impl FileAccess) -> RpcResult<&Path> {
        access.check(&self.0)?;
        Ok(Path::new(&self.0))
    }
}

/// Decides which files the server opens for a client.
pub trait FileAccess {
    /// Returns an error if the server must not open the file at `path`.
    fn check(&self, path: &str) -> RpcResult<()>;
}

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
