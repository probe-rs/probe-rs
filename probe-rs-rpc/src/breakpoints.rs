use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::{Key, RpcResult, Session};

#[derive(Clone, Debug, Serialize, Deserialize, Schema, PartialEq, Eq)]
pub enum WireColumn {
    LeftEdge,
    Column(u64),
}

#[derive(Clone, Debug, Serialize, Deserialize, Schema, PartialEq, Eq)]
pub struct WireSourceLocation {
    pub path: String,
    pub line: Option<u64>,
    pub column: Option<WireColumn>,
    pub address: Option<u64>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct SourceBreakpointLocation {
    pub path: String,
    pub line: u64,
    pub column: Option<u64>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct ResolveSourceBreakpointsRequest {
    pub sessid: Key<Session>,
    pub locations: Vec<SourceBreakpointLocation>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct WireVerifiedBreakpoint {
    pub address: u64,
    pub source_location: WireSourceLocation,
}

#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct BreakpointResolution {
    pub breakpoint: Option<WireVerifiedBreakpoint>,
    pub error: Option<String>,
}

pub type ResolveSourceBreakpointsResponse = RpcResult<Vec<BreakpointResolution>>;

#[derive(Serialize, Deserialize, Schema)]
pub struct ResolveSourceLocationsRequest {
    pub sessid: Key<Session>,
    pub addresses: Vec<u64>,
}

pub type ResolveSourceLocationsResponse = RpcResult<Vec<Option<WireSourceLocation>>>;
