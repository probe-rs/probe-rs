use std::time::Duration;

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::{
    Key, RpcResult, Session,
    probe::{AttachRequest, AttachResult},
};

#[derive(Serialize, Deserialize, Schema)]
pub struct ResetCoreRequest {
    pub sessid: Key<Session>,
    pub core: u32,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct ResetCoreAndHaltRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub timeout: Duration,
}

/// How `probe-rs reset` resets the target.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize, Deserialize, Schema)]
pub enum ResetMethod {
    /// Pulse the reset line of the probe.
    Pin,
    /// Reset the core through the debug interface.
    Core,
}

/// Opens the probe and resets the target.
#[derive(Serialize, Deserialize, Schema)]
pub struct ResetTargetRequest {
    /// How to open the probe. `resume_target` is not used.
    pub attach: AttachRequest,
    /// Overrides `reset_line_connected` of the probe settings, but `Pin` fails if it is false.
    pub method: Option<ResetMethod>,
    /// The core that a core reset resets.
    pub core: u32,
}

pub type ResetTargetResponse = RpcResult<AttachResult<()>>;
