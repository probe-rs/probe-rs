use nusb::descriptors::ActiveConfigurationError;

use crate::probe::{ftdi::ftdaye::ChipType, DebugProbeError};

#[derive(Debug, displaydoc::Display, thiserror::Error)]
#[ignore_extra_doc_attributes]
pub enum FtdiError {
    /// A USB transport error occurred.
    ///
    /// This variant is used for all errors reported by the operating system when performing a USB
    /// operation. It may indicate that the USB device was unplugged, that another application or an
    /// operating system driver is currently using it, or that the current user does not have
    /// permission to access it.
    Usb(#[from] nusb::Error),

    /// Unsupported chip type: {0:?}.
    /// The connected device is not supported by the driver.
    UnsupportedChipType(ChipType),

    /// Failed to get active configuration.
    ActiveConfigurationError(#[source] ActiveConfigurationError),

    /// An uncategorized error occurred: {0}
    Other(String),
}

impl From<FtdiError> for DebugProbeError {
    fn from(e: FtdiError) -> Self {
        DebugProbeError::ProbeSpecific(Box::new(e))
    }
}
