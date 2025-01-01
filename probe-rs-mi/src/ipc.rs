//! IPC data format.

use serde::{Deserialize, Serialize};

/// The probe-rs meta information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcData {
    /// The version of the current binary.
    pub version: semver::Version,
    /// The host of the local server.
    pub local_port: u16,
    /// The token used by local server conections.
    pub local_token: String,
}
