use std::time::Duration;

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::{Key, Session, probe::DebugProbeEntry};

/// The method that should be used for resetting the target.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum ResetMethod {
    /// The reset the target's debug sequence defines, usually a system reset.
    #[default]
    #[cfg_attr(feature = "clap", value(aliases(["sw"])))]
    Default,

    /// Drive the target's nRESET pin.
    #[cfg_attr(feature = "clap", value(aliases(["hw", "pin", "nreset"])))]
    Hardware,
}

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

/// A pin reset needs no target description, so it takes no chip and no core.
#[derive(Serialize, Deserialize, Schema)]
pub struct ResetHardwareRequest {
    pub probe: DebugProbeEntry,
    pub speed: Option<u32>,
    pub dry_run: bool,
}
