use std::time::Duration;

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::{
    Key, RpcResult, Session,
    probe::{DebugProbeEntry, WireProtocol},
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

/// Pulses the reset line of the probe, if the method or the probe settings say so. Needs no
/// target description.
#[derive(Serialize, Deserialize, Schema)]
pub struct ResetProbeRequest {
    pub probe: DebugProbeEntry,
    /// Overrides `reset_line_connected` of the probe settings.
    pub method: Option<ResetMethod>,
    /// The chip, to clear the debug state of its cores after the reset.
    pub chip: Option<String>,
    pub protocol: Option<WireProtocol>,
    /// How long to wait for a busy probe.
    pub wait_for_probe: Option<Duration>,
}

/// `false` if the reset line is not connected. The target is not reset then.
pub type ResetProbeResponse = RpcResult<bool>;
