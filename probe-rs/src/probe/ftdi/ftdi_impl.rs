#![allow(unused)]
// Ported from https://github.com/tanriol/ftdi-rs

use libftdi1_sys as ffi;

use std::convert::TryInto;
use std::io::{self, ErrorKind, Read, Write};

use std::ffi::CStr;
use std::{mem, ptr};

/// The target interface
pub enum Interface {
    A,
    B,
    C,
    D,
    Any,
}

impl From<Interface> for ffi::ftdi_interface {
    fn from(value: Interface) -> Self {
        match value {
            Interface::A => ffi::ftdi_interface::INTERFACE_A,
            Interface::B => ffi::ftdi_interface::INTERFACE_B,
            Interface::C => ffi::ftdi_interface::INTERFACE_C,
            Interface::D => ffi::ftdi_interface::INTERFACE_D,
            Interface::Any => ffi::ftdi_interface::INTERFACE_ANY,
        }
    }
}

pub enum BitMode {
    Reset,
    Bitbang,
    Mpsse,
    SyncBb,
    Mcu,
    Opto,
    Cbus,
    SyncFf,
    Ft1284,
}

impl From<BitMode> for ffi::ftdi_mpsse_mode {
    fn from(value: BitMode) -> Self {
        match value {
            BitMode::Reset => ffi::ftdi_mpsse_mode::BITMODE_RESET,
            BitMode::Bitbang => ffi::ftdi_mpsse_mode::BITMODE_BITBANG,
            BitMode::Mpsse => ffi::ftdi_mpsse_mode::BITMODE_MPSSE,
            BitMode::SyncBb => ffi::ftdi_mpsse_mode::BITMODE_SYNCBB,
            BitMode::Mcu => ffi::ftdi_mpsse_mode::BITMODE_MCU,
            BitMode::Opto => ffi::ftdi_mpsse_mode::BITMODE_OPTO,
            BitMode::Cbus => ffi::ftdi_mpsse_mode::BITMODE_CBUS,
            BitMode::SyncFf => ffi::ftdi_mpsse_mode::BITMODE_SYNCFF,
            BitMode::Ft1284 => ffi::ftdi_mpsse_mode::BITMODE_FT1284,
        }
    }
}

pub struct Builder {
    context: *mut ffi::ftdi_context,
}

impl Builder {
    pub fn new() -> Self {
        let context = unsafe { ffi::ftdi_new() };
        // Can be null on either OOM or libusb_init failure
        assert!(!context.is_null());

        Self { context }
    }

    pub fn set_interface(&mut self, interface: Interface) -> Result<()> {
        let result = unsafe { ffi::ftdi_set_interface(self.context, interface.into()) };
        match result {
            0 => Ok(()),
            -1 => unreachable!("unknown interface from ftdi.h"),
            -2 => unreachable!("missing context"),
            -3 => unreachable!("device already opened in Builder"),
            _ => Err(Error::unknown(self.context)),
        }
    }

    pub fn usb_open(mut self, vendor: u16, product: u16) -> Result<Device> {
        let result = unsafe { ffi::ftdi_usb_open(self.context, vendor as i32, product as i32) };
        match result {
            0 => Ok(Device {
                context: mem::replace(&mut self.context, ptr::null_mut()),
            }),
            -1 => Err(Error::EnumerationFailed), // usb_find_busses() failed
            -2 => Err(Error::EnumerationFailed), // usb_find_devices() failed
            -3 => Err(Error::DeviceNotFound),    // usb device not found
            -4 => Err(Error::AccessFailed),      // unable to open device
            -5 => Err(Error::ClaimFailed),       // unable to claim device
            -6 => Err(Error::RequestFailed),     // reset failed
            -7 => Err(Error::RequestFailed),     // set baudrate failed
            -8 => Err(Error::EnumerationFailed), // get product description failed
            -9 => Err(Error::EnumerationFailed), // get serial number failed
            -10 => Err(Error::unknown(self.context)), // unable to close device
            -11 => unreachable!("uninitialized context"), // ftdi context invalid
            -12 => Err(Error::EnumerationFailed), // libusb_get_device_list() failed
            _ => Err(Error::unknown(self.context)),
        }
    }
}

impl Drop for Builder {
    fn drop(&mut self) {
        if !self.context.is_null() {
            unsafe { ffi::ftdi_free(self.context) }
        }
    }
}

#[derive(Debug)]
pub struct Device {
    context: *mut ffi::ftdi_context,
}

impl Device {
    pub fn usb_reset(&mut self) -> Result<()> {
        let result = unsafe { ffi::ftdi_usb_reset(self.context) };
        match result {
            0 => Ok(()),
            -1 => Err(Error::RequestFailed),
            -2 => unreachable!("uninitialized context"),
            _ => Err(Error::unknown(self.context)),
        }
    }

    pub fn usb_purge_buffers(&mut self) -> Result<()> {
        let result = unsafe { ffi::ftdi_usb_purge_buffers(self.context) };
        match result {
            0 => Ok(()),
            -1 /* read */ | -2 /* write */ => Err(Error::RequestFailed),
            -3 => unreachable!("uninitialized context"),
            _ => Err(Error::unknown(self.context)),
        }
    }

    pub fn set_latency_timer(&mut self, value: u8) -> Result<()> {
        let result = unsafe { ffi::ftdi_set_latency_timer(self.context, value) };
        match result {
            0 => Ok(()),
            -1 => Err(Error::InvalidInput("latency value out of range")),
            -2 => Err(Error::RequestFailed),
            -3 => unreachable!("uninitialized context"),
            _ => Err(Error::unknown(self.context)),
        }
    }

    pub fn latency_timer(&mut self) -> Result<u8> {
        let mut value = 0u8;
        let result = unsafe { ffi::ftdi_get_latency_timer(self.context, &mut value) };
        match result {
            0 => Ok(value),
            -1 => Err(Error::RequestFailed),
            -2 => unreachable!("uninitialized context"),
            _ => Err(Error::unknown(self.context)),
        }
    }

    pub fn set_write_chunksize(&mut self, value: u32) {
        let result = unsafe { ffi::ftdi_write_data_set_chunksize(self.context, value) };
        match result {
            0 => (),
            -1 => unreachable!("uninitialized context"),
            err => panic!("unknown set_write_chunksize retval {:?}", err),
        }
    }

    pub fn write_chunksize(&mut self) -> u32 {
        let mut value = 0;
        let result = unsafe { ffi::ftdi_write_data_get_chunksize(self.context, &mut value) };
        match result {
            0 => value,
            -1 => unreachable!("uninitialized context"),
            err => panic!("unknown get_write_chunksize retval {:?}", err),
        }
    }

    pub fn set_read_chunksize(&mut self, value: u32) {
        let result = unsafe { ffi::ftdi_read_data_set_chunksize(self.context, value) };
        match result {
            0 => (),
            -1 => unreachable!("uninitialized context"),
            err => panic!("unknown set_write_chunksize retval {:?}", err),
        }
    }

    pub fn read_chunksize(&mut self) -> u32 {
        let mut value = 0;
        let result = unsafe { ffi::ftdi_read_data_get_chunksize(self.context, &mut value) };
        match result {
            0 => value,
            -1 => unreachable!("uninitialized context"),
            err => panic!("unknown get_write_chunksize retval {:?}", err),
        }
    }

    pub fn set_baudrate(&mut self, baudrate: i32) -> io::Result<()> {
        let result = unsafe { ffi::ftdi_set_baudrate(self.context, baudrate) };
        match result {
            0 => Ok(()),
            -1 => Err(io::Error::new(ErrorKind::InvalidInput, "invalid baudrate")),
            -2 => Err(io::Error::new(ErrorKind::Other, "setting baudrate failed")),
            -3 => Err(io::Error::new(ErrorKind::Other, "USB device unavailable")),
            _ => Err(io::Error::new(
                ErrorKind::Other,
                "unknown set baudrate error",
            )),
        }
    }

    pub fn set_bitmode(&mut self, bitmask: u8, mode: BitMode) -> io::Result<()> {
        let mode: ffi::ftdi_mpsse_mode = mode.into();
        let result = unsafe { ffi::ftdi_set_bitmode(self.context, bitmask, mode.0 as u8) };
        match result {
            0 => Ok(()),
            -1 => Err(io::Error::new(
                ErrorKind::Other,
                "can't enable bitbang mode",
            )),
            -2 => Err(io::Error::new(ErrorKind::Other, "USB device unavailable")),
            _ => Err(io::Error::new(
                ErrorKind::Other,
                "unknown set bitmode error",
            )),
        }
    }

    pub fn disable_bitbang(&mut self) -> io::Result<()> {
        let result = unsafe { ffi::ftdi_disable_bitbang(self.context) };
        match result {
            0 => Ok(()),
            -1 => Err(io::Error::new(
                ErrorKind::Other,
                "can't disable bitbang mode",
            )),
            -2 => Err(io::Error::new(ErrorKind::Other, "USB device unavailable")),
            _ => Err(io::Error::new(
                ErrorKind::Other,
                "unknown disable bitbang error",
            )),
        }
    }
}

unsafe impl Send for Device {}

impl Drop for Device {
    fn drop(&mut self) {
        let result = unsafe { ffi::ftdi_usb_close(self.context) };
        match result {
            0 => {}
            -1 => { /* TODO emit warning ("usb_release failed") */ }
            -3 => unreachable!("uninitialized context"),
            _ => panic!("undocumented ftdi_usb_close return value"),
        };
        unsafe {
            ffi::ftdi_free(self.context);
        }
    }
}

impl Read for Device {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().try_into().unwrap_or(std::i32::MAX);
        let result = unsafe { ffi::ftdi_read_data(self.context, buf.as_mut_ptr(), len) };
        match result {
            count if count >= 0 => Ok(count as usize),
            -666 => unreachable!("uninitialized context"),
            err => Err(libusb_to_io(err)),
        }
    }
}

impl Write for Device {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len().try_into().unwrap_or(std::i32::MAX);
        let result = unsafe { ffi::ftdi_write_data(self.context, buf.as_ptr(), len) };
        match result {
            count if count >= 0 => Ok(count as usize),
            -666 => unreachable!("uninitialized context"),
            err => Err(libusb_to_io(err)),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to enumerate devices to open the correct one")]
    EnumerationFailed,
    #[error("the specified device could not be found")]
    DeviceNotFound,
    #[error("failed to open the specified device")]
    AccessFailed,
    #[error("the requested interface could not be claimed")]
    ClaimFailed,
    #[error("the device has been disconnected from the system")]
    Disconnected,
    #[error("the device does not have the specified interface")]
    NoSuchInterface,
    #[error("libftdi reported error to perform operation")]
    RequestFailed,
    #[error("input value invalid: {0}")]
    InvalidInput(&'static str),
    #[error("I/O error: {0}")]
    Io(io::Error),

    #[error("unknown or unexpected libftdi error")]
    Unknown { source: LibFtdiError },
}

impl Error {
    pub(crate) fn unknown(context: *mut ffi::ftdi_context) -> Self {
        let message = unsafe { CStr::from_ptr(ffi::ftdi_get_error_string(context)) }
            .to_str()
            .expect("all error strings are expected to be ASCII");
        Error::Unknown {
            source: LibFtdiError { message },
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
#[error("libftdi: {message}")]
pub struct LibFtdiError {
    message: &'static str,
}

// Ideally this should be using libusb bindings, but we don't depend on any specific USB crate yet
pub(crate) fn libusb_to_io(code: i32) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("libusb error code {}", code))
}
