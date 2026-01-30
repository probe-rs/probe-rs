use nusb::ActiveConfigurationError;

use super::ChipType;
use crate::probe::ProbeError;

#[derive(Debug, thiserror::Error)]
pub enum FtdiError {
    #[error("A USB transport error occurred.")]
    ///
    /// This variant is used for all errors reported by the operating system when performing a USB
    /// operation. It may indicate that the USB device was unplugged, that another application or an
    /// operating system driver is currently using it, or that the current user does not have
    /// permission to access it.
    Usb(#[from] std::io::Error),

    #[error("Unsupported chip type: {0:?}")]
    /// The connected device is not supported by the driver.
    UnsupportedChipType(ChipType),

    #[error("Failed to get active configuration")]
    ActiveConfigurationError(#[source] ActiveConfigurationError),

    #[error("{0}")]
    /// An unspecified error occurred.
    Other(String),
}

impl ProbeError for FtdiError {}
