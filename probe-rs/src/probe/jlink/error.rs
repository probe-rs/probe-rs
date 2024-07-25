use crate::probe::DebugProbeError;

use super::{capabilities::Capability, interface::Interface};

#[derive(Debug, thiserror::Error)]
pub enum JlinkError {
    #[error("Unknown interface reported by J-Link: {0:?}")]
    UnknownInterface(Interface),

    #[error("A USB transport error occurred.")]
    ///
    /// This variant is used for all errors reported by the operating system when performing a USB
    /// operation. It may indicate that the USB device was unplugged, that another application or an
    /// operating system driver is currently using it, or that the current user does not have
    /// permission to access it.
    Usb(#[from] nusb::Error),

    #[error("device is missing capabilities ({0:?}) for operation")]
    /// An operation was attempted that is not supported by the probe.
    ///
    /// Some operations are not supported by all firmware/hardware versions, and are instead
    /// advertised as optional *capability* bits. This error occurs when the capability bit for an
    /// operation isn't set when that operation is attempted.
    ///
    /// Capabilities can be read by calling [`super::JLink::capabilities()`], which returns a
    /// [`Capabilities`][super::Capabilities] struct.
    MissingCapability(Capability),

    #[error("probe does not support target interface {0:?}")]
    InterfaceNotSupported(Interface),

    #[error("interface {needed:?} must be selected for this operation (currently using interface {selected:?})")]
    WrongInterfaceSelected {
        selected: Interface,
        needed: Interface,
    },

    #[error("{0}")]
    Other(String),
}

impl From<JlinkError> for DebugProbeError {
    fn from(e: JlinkError) -> Self {
        DebugProbeError::ProbeSpecific(Box::new(e))
    }
}
