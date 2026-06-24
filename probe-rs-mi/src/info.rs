//! Used to provide device information.
//!
//! Returned by `probe-rs mi info`

use serde::{Deserialize, Serialize};

/// The probe-rs meta information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasicDeviceInfo {
    /// The chip name, as accepted by `--chip` options of `probe-rs`.
    pub chip: String,
}
