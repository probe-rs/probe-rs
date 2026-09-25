use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::{Key, Session};

#[derive(Serialize, Deserialize, Schema)]
pub struct WriteMemoryRequest<W> {
    pub sessid: Key<Session>,
    pub core: u32,
    pub address: u64,
    pub data: Vec<W>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct ReadMemoryRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub address: u64,
    pub count: u32,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct ReadBytesRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub address: u64,
    pub count: u64,
}
