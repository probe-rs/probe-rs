use std::fmt;

type BoxedError = Box<dyn std::error::Error + Send + Sync>;

#[allow(unused_imports)] // for intra-doc links
use super::{scan_usb, Capabilities, JayLink};

/// List of specific errors that may occur when using this library.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// A USB transport error occurred.
    ///
    /// This variant is used for all errors reported by the operating system when performing a USB
    /// operation. It may indicate that the USB device was unplugged, that another application or an
    /// operating system driver is currently using it, or that the current user does not have
    /// permission to access it.
    Usb,

    /// No (matching) J-Link device was found.
    ///
    /// This error occurs when calling [`JayLink::open_by_serial`] while no J-Link device is connected
    /// (or no device matching the serial number is connected).
    DeviceNotFound,

    /// Automatic device connection failed because multiple devices were found.
    ///
    /// This error occurs when calling [`JayLink::open_by_serial`] without a serial number while
    /// multiple J-Link devices are connected. This library will refuse to "guess" a device and
    /// requires specifying a serial number in this case. The [`scan_usb`] function can also be used
    /// to find a specific device to connect to.
    MultipleDevicesFound,

    /// A operation was attempted that is not supported by the probe.
    ///
    /// Some operations are not supported by all firmware/hardware versions, and are instead
    /// advertised as optional *capability* bits. This error occurs when the capability bit for an
    /// operation isn't set when that operation is attempted.
    ///
    /// Capabilities can be read by calling [`JayLink::capabilities`], which returns a
    /// [`Capabilities`] bitflags struct.
    MissingCapability,

    /// The device does not support the selected target interface.
    InterfaceNotSupported,

    /// An unspecified error occurred.
    Other,
}

pub(crate) trait Cause {
    const KIND: ErrorKind;
}

/// The error type used by this library.
///
/// Errors can be introspected by the user by calling [`Error::kind`] and inspecting the returned
/// [`ErrorKind`].
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    inner: BoxedError,
    while_: Option<&'static str>,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, inner: impl Into<BoxedError>) -> Self {
        Self {
            kind,
            inner: inner.into(),
            while_: None,
        }
    }

    pub(crate) fn with_while(
        kind: ErrorKind,
        inner: impl Into<BoxedError>,
        while_: &'static str,
    ) -> Self {
        Self {
            kind,
            inner: inner.into(),
            while_: Some(while_),
        }
    }

    fn fmt_while(&self) -> String {
        if let Some(while_) = self.while_ {
            format!(" while {}", while_)
        } else {
            String::new()
        }
    }

    /// Returns the [`ErrorKind`] describing this error.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Prefix foreign errors with further explanation where they're coming from
        match self.kind {
            ErrorKind::Usb => write!(f, "USB error{}: {}", self.fmt_while(), self.inner),
            _ => {
                if let Some(while_) = self.while_ {
                    write!(f, "error{}: {}", while_, self.inner)
                } else {
                    self.inner.fmt(f)
                }
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

pub(crate) trait ResultExt<T, E> {
    fn jaylink_err(self) -> Result<T, Error>
    where
        E: Cause + Into<BoxedError>;

    fn jaylink_err_while(self, while_: &'static str) -> Result<T, Error>
    where
        E: Cause + Into<BoxedError>;
}

impl<T, E> ResultExt<T, E> for Result<T, E> {
    fn jaylink_err(self) -> Result<T, Error>
    where
        E: Cause + Into<BoxedError>,
    {
        self.map_err(|e| Error::new(E::KIND, e))
    }

    fn jaylink_err_while(self, while_: &'static str) -> Result<T, Error>
    where
        E: Cause + Into<BoxedError>,
    {
        self.map_err(|e| Error::with_while(E::KIND, e, while_))
    }
}

macro_rules! error_mapping {
    ($errty:ty => $kind:ident) => {
        impl Cause for $errty {
            const KIND: ErrorKind = ErrorKind::$kind;
        }
    };
}

error_mapping!(std::io::Error => Usb);
error_mapping!(nusb::transfer::TransferError => Usb);
error_mapping!(String => Other);
