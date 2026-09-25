use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::{Key, RpcResult, Session};

#[derive(Serialize, Deserialize, Schema)]
pub struct ScopesRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub frame_id: u32,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct WireScope {
    pub name: String,
    pub presentation_hint: Option<String>,
    pub variables_reference: i64,
    pub expensive: bool,
    pub line: Option<i64>,
    pub column: Option<i64>,
}

pub type ScopesResponse = RpcResult<Vec<WireScope>>;

#[derive(Serialize, Deserialize, Schema)]
pub struct VariablesRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub variables_reference: u32,
    pub filter: Option<String>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct ClearCoreDebugStateRequest {
    pub sessid: Key<Session>,
    pub core: u32,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct LoadSvdRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    /// Server-side path to the CMSIS-SVD file (the client uploads it via the
    /// temp-file endpoints, then passes the resulting path here), or `None`
    /// to remove the core's current SVD state.
    pub path: Option<String>,
}

pub type LoadSvdResponse = RpcResult<()>;

#[derive(Serialize, Deserialize, Schema)]
pub struct EvaluateRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub frame_id: Option<u32>,
    pub expression: String,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct WireEvaluateResponse {
    pub result: String,
    pub type_: Option<String>,
    pub variables_reference: i64,
    pub named_variables: Option<i64>,
    pub indexed_variables: Option<i64>,
    pub memory_reference: Option<String>,
}

pub type EvaluateResponse = RpcResult<WireEvaluateResponse>;

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct WireVariable {
    pub name: String,
    pub evaluate_name: Option<String>,
    pub memory_reference: Option<String>,
    pub indexed_variables: Option<i64>,
    pub named_variables: Option<i64>,
    pub type_: Option<String>,
    pub value: String,
    pub variables_reference: i64,
}

pub type VariablesResponse = RpcResult<Vec<WireVariable>>;

#[derive(Serialize, Deserialize, Schema)]
pub struct SetVariableRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub parent_key: i64,
    pub name: String,
    pub value: String,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct WireSetVariableResponse {
    pub value: String,
    pub type_: Option<String>,
    pub variables_reference: i64,
    pub named_variables: Option<i64>,
    pub indexed_variables: Option<i64>,
    pub memory_reference: Option<String>,
}

pub type SetVariableResult = RpcResult<WireSetVariableResponse>;
