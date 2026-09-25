use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::{Key, RpcResult, Session};

#[derive(Serialize, Deserialize, Schema)]
pub struct DisassembleRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub memory_reference: u64,
    pub byte_offset: i64,
    pub instruction_offset: i64,
    pub instruction_count: i64,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct WireSource {
    pub name: Option<String>,
    pub path: Option<String>,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct WireDisassembledInstruction {
    pub address: String,
    pub column: Option<i64>,
    pub instruction: String,
    pub instruction_bytes: Option<String>,
    pub line: Option<i64>,
    pub location: Option<WireSource>,
}

pub type DisassembleResponse = RpcResult<Vec<WireDisassembledInstruction>>;
