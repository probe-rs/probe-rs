use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::{Key, RpcResult, TempFileHandle};

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
