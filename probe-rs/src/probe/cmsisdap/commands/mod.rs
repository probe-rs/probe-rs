pub mod edbg;
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
    SwoBaudrateNotConfigured,
    #[error("Probe reported an error while streaming SWO")]
    SwoTraceStreamError,
    #[error("Requested SWO mode is not available on this probe")]
    SwoModeNotAvailable,
    #[error("Error in the USB HID access")]
    HidApi(#[from] hidapi::HidError),
    #[error("Error in the USB access")]
    UsbError(#[from] rusb::Error),
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

pub enum CmsisDapDevice {
    /// CMSIS-DAP v1 over HID.
    /// Stores a HID device handle and maximum HID report size.
    V1 {
        handle: hidapi::HidDevice,
        report_size: usize,
    },

    /// CMSIS-DAP v2 over WinUSB/Bulk.
    /// Stores an rusb device handle, out/in EP addresses, maximum DAP packet size,
    /// and an optional SWO streaming EP address and SWO maximum packet size.
    V2 {
        handle: rusb::DeviceHandle<rusb::Context>,
        out_ep: u8,
        in_ep: u8,
        max_packet_size: usize,
        swo_ep: Option<(u8, usize)>,
    },
}

impl CmsisDapDevice {
    /// Read from the probe into `buf`, returning the number of bytes read on success.
    fn read(&self, buf: &mut [u8]) -> Result<usize> {
        match self {
            CmsisDapDevice::V1 { handle, .. } => Ok(handle.read_timeout(buf, 100)?),
            CmsisDapDevice::V2 { handle, in_ep, .. } => {
                let timeout = Duration::from_millis(100);
                Ok(handle.read_bulk(*in_ep, buf, timeout)?)
            }
        }
    }

    /// Write `buf` to the probe, returning the number of bytes written on success.
    fn write(&self, buf: &[u8]) -> Result<usize> {
        match self {
            CmsisDapDevice::V1 { handle, .. } => Ok(handle.write(buf)?),
            CmsisDapDevice::V2 { handle, out_ep, .. } => {
                let timeout = Duration::from_millis(100);
                // Skip first byte as it's set to 0 for HID transfers
                Ok(handle.write_bulk(*out_ep, &buf[1..], timeout)?)
            }
        }
    }

    pub(super) fn drain(&self) {
        log::debug!("Draining probe of any pending data.");

        match self {
            CmsisDapDevice::V1 {
                handle,
                report_size,
                ..
            } => loop {
                let mut discard = vec![0u8; report_size + 1];
                match handle.read_timeout(&mut discard, 1) {
                    Ok(n) if n != 0 => continue,
                    _ => break,
                }
            },

            CmsisDapDevice::V2 {
                handle,
                in_ep,
                max_packet_size,
                ..
            } => {
                let timeout = Duration::from_millis(1);
                let mut discard = vec![0u8; *max_packet_size];
                loop {
                    match handle.read_bulk(*in_ep, &mut discard, timeout) {
                        Ok(n) if n != 0 => continue,
                        _ => break,
                    }
                }
            }
        }
    }

    /// Check if SWO streaming is supported by this device.
    pub(super) fn swo_streaming_supported(&self) -> bool {
        match self {
            CmsisDapDevice::V1 { .. } => false,
            CmsisDapDevice::V2 { swo_ep, .. } => swo_ep.is_some(),
        }
    }

    /// Read from the SWO streaming endpoint.
    ///
    /// Returns SWOModeNotAvailable if this device does not support SWO streaming.
    ///
    /// On timeout, returns a zero-length buffer.
    pub(super) fn read_swo_stream(&self, timeout: Duration) -> Result<Vec<u8>> {
        match self {
            CmsisDapDevice::V1 { .. } => Err(CmsisDapError::SwoModeNotAvailable.into()),
            CmsisDapDevice::V2 { handle, swo_ep, .. } => match swo_ep {
                Some((ep, len)) => {
                    let mut buf = vec![0u8; *len];
                    match handle.read_bulk(*ep, &mut buf, timeout) {
                        Ok(n) => {
                            buf.truncate(n);
                            Ok(buf)
                        }
                        Err(rusb::Error::Timeout) => {
                            buf.truncate(0);
                            Ok(buf)
                        }
                        Err(e) => Err(e.into()),
                    }
                }
                None => Err(CmsisDapError::SwoModeNotAvailable.into()),
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

    /// Convert the request to bytes, which can be sent to the probe.
    /// Returns the amount of bytes written to the buffer.
    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize>;
}

pub(crate) trait Response: Sized {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self>;
}

pub(crate) fn send_command<Req: Request, Res: Response>(
    device: &mut CmsisDapDevice,
    request: Req,
) -> Result<Res> {
    // Size the buffer for the maximum packet size.
    // On v1, we always send this full-sized report, while
    // on v2 we can truncate to just the required data.
    // Add one byte for HID report ID.
    let buffer_len: usize = match device {
        CmsisDapDevice::V1 { report_size, .. } => *report_size + 1,
        CmsisDapDevice::V2 {
            max_packet_size, ..
        } => *max_packet_size + 1,
    };
    let mut buffer = vec![0; buffer_len];

    // Leave byte 0 as the HID report, and write the command and request to the buffer.
    buffer[1] = *Req::CATEGORY;
    let mut size = request.to_bytes(&mut buffer, 2)? + 2;

    // For HID devices we must write a full report every time,
    // so set the transfer size to the report size, plus one
    // byte for the HID report ID. On v2 devices, we just
    // write the exact required size every time.
    if let CmsisDapDevice::V1 { report_size, .. } = device {
        size = *report_size + 1;
    }

    // Send buffer to the device.
    device.write(&buffer[..size])?;
    trace_buffer("Transmit buffer", &buffer[..size]);

    // Read back resonse.
    device.read(&mut buffer)?;
    trace_buffer("Receive buffer", &buffer[..]);

    if buffer[0] == *Req::CATEGORY {
        Res::from_bytes(&buffer, 1)
    } else {
        Err(anyhow!(CmsisDapError::UnexpectedAnswer))
            .with_context(|| format!("Received invalid data for {:?}", *Req::CATEGORY))
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
