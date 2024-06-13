//! Used to provide meta information.
//!
//! Returned by `probe-rs meta`

use serde::{Deserialize, Serialize};

/// The probe-rs meta information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    /// The version of the current binary.
    pub version: semver::Version,
    /// The commit of the current binary.
    pub commit: &'static str,
    /// The arch the current binary is built for.
    pub arch: &'static str,
    /// The OS the current binary is built for.
    pub os: &'static str,
}
