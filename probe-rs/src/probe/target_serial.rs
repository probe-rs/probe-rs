//! Serial ports to the target, for example the CDC-ACM port of a debug probe.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The state of a modem control line (DTR or RTS).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineState {
    /// Do not change the line.
    #[default]
    Keep,
    /// Deassert the line.
    Low,
    /// Assert the line.
    High,
}

/// Settings of a target serial port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialSettings {
    /// The baud rate. USB CDC devices often ignore it.
    pub baud: u32,
    /// The state of DTR after open.
    pub dtr: LineState,
    /// The state of RTS after open.
    pub rts: LineState,
}

impl Default for SerialSettings {
    fn default() -> Self {
        Self {
            baud: 115_200,
            dtr: LineState::Keep,
            rts: LineState::Keep,
        }
    }
}

/// An error of a target serial port.
#[derive(Debug, thiserror::Error)]
pub enum TargetSerialError {
    /// No serial port matches.
    #[error("No serial port matches {0}")]
    NotFound(String),
    /// More than one serial port matches.
    #[error("More than one serial port matches {0}")]
    Ambiguous(String),
    /// The port is not open.
    #[error("The serial port is not open")]
    NotOpen,
    /// An I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A serial port to the target.
///
/// A debug probe gives the port with [`DebugProbe::target_serial`](super::DebugProbe::target_serial)
/// before attach. After that, the port does not depend on the probe or the session. The port is
/// closed until [`open`](Self::open). To disable the port, do not open it.
pub trait TargetSerial: Send + fmt::Debug {
    /// The settings that the driver recommends. The caller can change them before [`open`](Self::open).
    fn default_settings(&self) -> SerialSettings;

    /// Opens the port and applies the line states.
    fn open(&mut self, settings: &SerialSettings) -> Result<(), TargetSerialError>;

    /// Reads received data. Does not block. Returns 0 if no data is available.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TargetSerialError>;

    /// Writes data to the target.
    fn write(&mut self, data: &[u8]) -> Result<usize, TargetSerialError>;
}
