pub mod general;
pub mod swd;
pub mod swj;
pub mod swo;
pub mod transfer;

use crate::architecture::arm::DapError;
use crate::DebugProbeError;
use core::ops::Deref;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use log::log_enabled;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CmsisDapError {
    #[error("Unexpected answer to command")]
    UnexpectedAnswer,
    #[error("CMSIS-DAP responded with an error")]
    ErrorResponse,
    #[error("Too much data provided for SWJ Sequence command")]
    TooMuchData,
    #[error("Not enough data in response from probe")]
    NotEnoughData,
    #[error("Requested SWO baud rate could not be configured")]
    SWOBaudrateNotConfigured,
    #[error("Probe reported an error while streaming SWO")]
    SWOTraceStreamError,
    #[error("Requested SWO mode is not available on this probe")]
    SWOModeNotAvailable,
    #[error("Error in the USB HID access")]
    HidApi(#[from] hidapi::HidError),
    #[error("Error in the USB access")]
    USBError(#[from] rusb::Error),
    #[error("An error with the DAP communication occured")]
    Dap(#[from] DapError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<CmsisDapError> for DebugProbeError {
    fn from(error: CmsisDapError) -> Self {
        DebugProbeError::ProbeSpecific(Box::new(error))
    }
}

pub enum DAPLinkDevice {
    /// CMSIS-DAP v1 over HID. Stores a HID device handle.
    V1(hidapi::HidDevice),

    /// CMSIS-DAP v2 over WinUSB/Bulk. Stores an rusb device handle and out/in EP addresses.
    V2 {
        handle: rusb::DeviceHandle<rusb::Context>,
        out_ep: u8,
        in_ep: u8,
        swo_ep: Option<u8>,
    },
}

impl DAPLinkDevice {
    /// Read from the probe into `buf`, returning the number of bytes read on success.
    fn read(&self, buf: &mut [u8]) -> Result<usize> {
        match self {
            DAPLinkDevice::V1(device) => Ok(device.read_timeout(buf, 100)?),
            DAPLinkDevice::V2 {
                handle,
                out_ep: _,
                in_ep,
                swo_ep: _,
            } => {
                let timeout = Duration::from_millis(100);
                Ok(handle.read_bulk(*in_ep, buf, timeout)?)
            }
        }
    }

    /// Write `buf` to the probe, returning the number of bytes written on success.
    fn write(&self, buf: &[u8]) -> Result<usize> {
        match self {
            DAPLinkDevice::V1(device) => Ok(device.write(buf)?),
            DAPLinkDevice::V2 {
                handle,
                out_ep,
                in_ep: _,
                swo_ep: _,
            } => {
                let timeout = Duration::from_millis(100);
                // Skip first byte as it's set to 0 for HID transfers
                Ok(handle.write_bulk(*out_ep, &buf[1..], timeout)?)
            }
        }
    }

    /// Check if SWO streaming is supported by this device.
    pub(super) fn swo_streaming_supported(&self) -> bool {
        match self {
            DAPLinkDevice::V1(_) => false,
            DAPLinkDevice::V2 { swo_ep, .. } => swo_ep.is_some(),
        }
    }

    /// Read into `buf` from the SWO streaming endpoint.
    ///
    /// Returns SWOModeNotAvailable if this device does not support SWO streaming.
    ///
    /// On timeout, returns Ok(0).
    pub(super) fn read_swo_stream(&self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        match self {
            DAPLinkDevice::V1(_) => Err(CmsisDapError::SWOModeNotAvailable.into()),
            DAPLinkDevice::V2 { handle, swo_ep, .. } => match swo_ep {
                Some(ep) => match handle.read_bulk(*ep, buf, timeout) {
                    Ok(n) => Ok(n),
                    Err(rusb::Error::Timeout) => Ok(0),
                    Err(e) => Err(e.into()),
                },
                None => Err(CmsisDapError::SWOModeNotAvailable.into()),
            },
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub(crate) enum Status {
    DAPOk = 0x00,
    DAPError = 0xFF,
}

impl Status {
    pub fn from_byte(value: u8) -> Result<Self> {
        match value {
            0x00 => Ok(Status::DAPOk),
            0xFF => Ok(Status::DAPError),
            _ => Err(CmsisDapError::UnexpectedAnswer).context("Status can only be 0x00 or 0xFF"),
        }
    }
}

pub(crate) struct Category(u8);

impl Deref for Category {
    type Target = u8;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub(crate) trait Request {
    const CATEGORY: Category;

    /// Convert the request to bytes, which can be sent to the Daplink. Returns
    /// the amount of bytes written to the buffer.
    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize>;
}

pub(crate) trait Response: Sized {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self>;
}

pub(crate) fn send_command<Req: Request, Res: Response>(
    device: &mut std::sync::Mutex<DAPLinkDevice>,
    request: Req,
) -> Result<Res> {
    // On CMSIS-DAP v2 USB HS devices, a single request might be up to 1024 bytes,
    // plus we need one extra byte for the always-written HID report ID.
    const BUFFER_LEN: usize = 1025;

    // Write the command & request to the buffer.
    let mut write_buffer = [0; BUFFER_LEN];
    write_buffer[1] = *Req::CATEGORY;
    let mut size = request.to_bytes(&mut write_buffer, 1 + 1)?;
    size += 2;

    if let Ok(device) = device.get_mut() {
        // On Windows, HID writes must write exactly the size of the
        // largest report for the device, but there's no way to query
        // this in hidapi. Almost all known CMSIS-DAP devices use 64-byte
        // HID reports (the maximum permitted), so ensure we always
        // write exactly 64 (+1 for report ID) bytes for HID.
        // For v2 devices, we can write the precise request size.
        if let DAPLinkDevice::V1(_) = device {
            size = 65;
        }

        // Send buffer to the device.
        device.write(&write_buffer[..size])?;
        trace_buffer("Transmit buffer", &write_buffer[..size]);

        // Read back resonse.
        let mut read_buffer = [0; BUFFER_LEN];
        device.read(&mut read_buffer)?;
        trace_buffer("Receive buffer", &read_buffer[..]);

        if read_buffer[0] == *Req::CATEGORY {
            Res::from_bytes(&read_buffer, 1)
        } else {
            Err(anyhow!(CmsisDapError::UnexpectedAnswer))
                .with_context(|| format!("Received invalid data for {:?}", *Req::CATEGORY))
        }
    } else {
        Err(anyhow!(CmsisDapError::ErrorResponse)).context("failed while sending command")
    }
}

/// Trace log a buffer, including only the first trailing zero.
///
/// This is useful for the CMSIS-DAP USB buffers, which often contain many trailing
/// zeros required for the various USB APIs, but make the trace output very long and
/// difficult to read.
fn trace_buffer(name: &str, buf: &[u8]) {
    if log_enabled!(log::Level::Trace) {
        let len = buf.len();
        let cut = len + 1 - buf.iter().rev().position(|&x| x != 0).unwrap_or(len);
        let end = std::cmp::min(len, std::cmp::max(1, cut));
        log::trace!("{}: {:02X?}...", name, &buf[..end]);
    }
}
