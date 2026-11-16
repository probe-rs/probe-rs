use nusb::ActiveConfigurationError;

use crate::probe::{
    ProbeError,
    ftdi::ftdaye::{ChipType, Interface},
};

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

    #[error(
        "The {chip_type:?} has no MPSSE on channel {channel:?} (interface {}). Use channel {}.",
        .channel.usb_interface(),
        list_mpsse_channels(*.chip_type)
    )]
    /// The selected channel of the chip has no MPSSE.
    NoMpsse {
        chip_type: ChipType,
        channel: Interface,
    },

    #[error("Failed to get active configuration")]
    ActiveConfigurationError(#[source] ActiveConfigurationError),

    #[error("{0}")]
    /// An unspecified error occurred.
    Other(String),
}

fn list_mpsse_channels(chip_type: ChipType) -> String {
    let channels: Vec<_> = chip_type
        .mpsse_channels()
        .iter()
        .map(|channel| format!("{channel:?} (interface {})", channel.usb_interface()))
        .collect();
    channels.join(" or ")
}

impl ProbeError for FtdiError {}

#[cfg(test)]
mod tests {
    use super::{ChipType, FtdiError, Interface};

    #[test]
    fn no_mpsse_error_names_the_mpsse_channels() {
        let error = FtdiError::NoMpsse {
            chip_type: ChipType::FT4232H,
            channel: Interface::C,
        };
        assert_eq!(
            error.to_string(),
            "The FT4232H has no MPSSE on channel C (interface 2). \
             Use channel A (interface 0) or B (interface 1)."
        );
    }
}
