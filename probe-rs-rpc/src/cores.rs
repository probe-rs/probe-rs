use std::time::Duration;

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::core_ops::WireCoreStatus;
use crate::{Key, RpcResult, Session};

/// Select which cores a batch operation applies to.
///
/// When `cores` is `None`, every core in the session is considered. When it is
/// `Some`, only the listed indices take part. Disabled cores are omitted from
/// the response rather than reported as errors.
#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct CoresRequest {
    pub sessid: Key<Session>,
    pub cores: Option<Vec<u32>>,
}

/// Halt selected cores.
#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct HaltCoresRequest {
    pub sessid: Key<Session>,
    pub cores: Option<Vec<u32>>,
    pub timeout: Duration,
}

/// Status of each active core that took part in a batch core operation.
#[derive(Serialize, Deserialize, Schema, Clone, Debug, Default)]
pub struct CoresStatusMap {
    /// `(core_index, status)` pairs for enabled cores.
    pub statuses: Vec<(u32, WireCoreStatus)>,
}

pub type CoresStatusResponse = RpcResult<CoresStatusMap>;
