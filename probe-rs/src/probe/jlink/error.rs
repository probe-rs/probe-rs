use crate::probe::ProbeError;

use super::{capabilities::Capability, interface::Interface, Command};

#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum JlinkError {
    /// A USB transport error occurred.
    #[ignore_extra_doc_attributes]
    ///
    /// This variant is used for all errors reported by the operating system when performing a USB
    /// operation. It may indicate that the USB device was unplugged, that another application or an
    /// operating system driver is currently using it, or that the current user does not have
    /// permission to access it.
    Usb(#[from] nusb::Error),

    /// The attempted operation is not possible because the device is missing capabilities ({0:?}).
    #[ignore_extra_doc_attributes]
    ///
    /// Some operations are not supported by all firmware/hardware versions, and are instead
    /// advertised as optional *capability* bits. This error occurs when the capability bit for an
    /// operation isn't set when that operation is attempted.
    ///
    /// Capabilities can be read by calling [`super::JLink::capabilities()`], which returns a
    /// [`Capabilities`][super::Capabilities] struct.
    MissingCapability(Capability),

    /// The probe does not support target interface {0:?}
    InterfaceNotSupported(Interface),

    /// Interface {needed:?} must be selected for this operation (currently using interface {selected:?})
    WrongInterfaceSelected {
        selected: Interface,
        needed: Interface,
    },

    /// Error while reading from device.
    ReadError(#[from] ReadError),

    /// Error while writing to device.
    WriteCommandError(#[from] WriteCommandError),

    /// {0}
    Other(String),
}

impl ProbeError for JlinkError {}

#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum ReadError {
    /// A USB transport error occurred.
    Usb(#[from] nusb::Error),
}

#[derive(Debug, thiserror::Error, docsplay::Display)]
/// Failed to write {command:?} with payload {print_bytes_truncated(payload)}
pub struct WriteCommandError {
    pub command: Command,
    pub payload: Vec<u8>,
    #[source]
    pub source: WriteCommandErrorKind,
}

#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum WriteCommandErrorKind {
    /// Incomplete write (expected {expected} bytes, wrote {written})
    IncompleteWrite { expected: usize, written: usize },

    /// A USB transport error occurred.
    Usb(#[from] nusb::Error),
}

fn print_bytes_truncated(bytes: &[u8]) -> String {
    fn print_bytes(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(", ")
    }

    const LIMIT: usize = 10;
    let exact = bytes.len() <= LIMIT + 1;

    if exact {
        format!("[{}]", print_bytes(bytes))
    } else {
        format!(
            "[{}, and {} more...]",
            print_bytes(&bytes[..LIMIT]),
            bytes.len() - LIMIT
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_print_bytes_truncated() {
        let bytes = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(
            print_bytes_truncated(&bytes),
            "[00, 01, 02, 03, 04, 05, 06, 07, 08, 09, 0A]"
        );
    }

    #[test]
    fn test_print_bytes_truncated_plus_one() {
        let bytes = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        assert_eq!(
            print_bytes_truncated(&bytes),
            "[00, 01, 02, 03, 04, 05, 06, 07, 08, 09, and 2 more...]"
        );
    }
}
