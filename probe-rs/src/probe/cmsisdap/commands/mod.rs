pub mod general;
pub mod swd;
pub mod swj;
pub mod swo;
pub mod transfer;

use crate::architecture::arm::DapError;
use crate::DebugProbeError;
use core::ops::Deref;
use general::info::{Command, PacketSize};
use std::time::Duration;

use log::log_enabled;

#[derive(Debug, thiserror::Error)]
pub enum CmsisDapError {
    #[error(transparent)]
    Send(#[from] SendError),
    #[error("CMSIS-DAP responded with an error")]
    ErrorResponse,
    #[error("Too much data provided for SWJ Sequence command")]
    TooMuchData,
    #[error("Requested SWO baud rate could not be configured")]
    SwoBaudrateNotConfigured,
    #[error("Probe reported an error while streaming SWO")]
    SwoTraceStreamError,
    #[error("Requested SWO mode is not available on this probe")]
    SwoModeNotAvailable,
    #[error("Could not determine a suitable report size for this probe")]
    NoReportSize,
    #[error("An error with the DAP communication occured")]
    Dap(#[from] DapError),
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("Error in the USB HID access")]
    HidApi(#[from] hidapi::HidError),
    #[error("Error in the USB access")]
    UsbError(rusb::Error),
    #[error("Not enough data in response from probe")]
    NotEnoughData,
    #[error("Status can only be 0x00 or 0xFF")]
    NonZeroOrFF,
    #[error("Connecting to target failed, received: {0:x}")]
    ConnectResponseError(u8),
    #[error("Received invalid data for {0:?}")]
    InvalidDataFor(u8),
    #[error("Unexpected answer to command")]
    UnexpectedAnswer,
    #[error("Failed to write word at data_offset {0}. This is a bug. Please report it.")]
    WriteToOffsetBug(usize),
    #[error("Timeout in USB communication.")]
    Timeout,
    #[error("This is a bug. Please report it.")]
    Bug,
}

impl From<rusb::Error> for SendError {
    fn from(error: rusb::Error) -> Self {
        match error {
            rusb::Error::Timeout => SendError::Timeout,
            other => SendError::UsbError(other),
        }
    }
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
    fn read(&self, buf: &mut [u8]) -> Result<usize, SendError> {
        match self {
            CmsisDapDevice::V1 { handle, .. } => match handle.read_timeout(buf, 100)? {
                // Timeout is not indicated by error, but by returning 0 read bytes
                0 => Err(SendError::Timeout),
                n => Ok(n),
            },
            CmsisDapDevice::V2 { handle, in_ep, .. } => {
                let timeout = Duration::from_millis(100);
                Ok(handle.read_bulk(*in_ep, buf, timeout)?)
            }
        }
    }

    /// Write `buf` to the probe, returning the number of bytes written on success.
    fn write(&self, buf: &[u8]) -> Result<usize, SendError> {
        match self {
            CmsisDapDevice::V1 { handle, .. } => Ok(handle.write(buf)?),
            CmsisDapDevice::V2 { handle, out_ep, .. } => {
                let timeout = Duration::from_millis(100);
                // Skip first byte as it's set to 0 for HID transfers
                Ok(handle.write_bulk(*out_ep, &buf[1..], timeout)?)
            }
        }
    }

    /// Drain any pending data from the probe, ensuring future responses are
    /// synchronised to requests. Swallows any errors, which are expected if
    /// there is no pending data to read.
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

    /// Determine the correct packet size for this device.
    ///
    /// The resulting size is set in the device (either max_packet_size for V2 devices,
    /// or report_size for V1 devices) and returned.
    pub(super) fn set_packet_size(&mut self) -> Result<usize, CmsisDapError> {
        // For V2 devices, we can always immediately send a packet asing what
        // its maximum packet size is. For V1 devices on Linux and Windows,
        // we can send a small 64-byte HID report to ask, and then later
        // use a larger HID report if appropriate. For V1 devices on MacOS,
        // we must always use the correct HID report size, but there's no
        // way to find out what it is, so we attempt all known sizes.
        for candidate in &[64, 512, 1024] {
            if let CmsisDapDevice::V1 {
                ref mut report_size,
                ..
            } = self
            {
                log::trace!("Attempting HID report size of {} bytes", candidate);
                *report_size = *candidate;
            }

            match send_command(self, Command::PacketSize) {
                Ok(PacketSize(packet_size)) => {
                    log::debug!("Configuring probe to use packet size {}", packet_size);
                    match self {
                        CmsisDapDevice::V1 {
                            ref mut report_size,
                            ..
                        } => {
                            *report_size = packet_size as usize;
                        }
                        CmsisDapDevice::V2 {
                            ref mut max_packet_size,
                            ..
                        } => {
                            *max_packet_size = packet_size as usize;
                        }
                    }
                    return Ok(packet_size as usize);
                }

                Err(e) => match self {
                    // Ignore errors on V1, because we expect them while
                    // trying various report sizes.
                    CmsisDapDevice::V1 { .. } => (),

                    // Escalate errors on V2 which is expected to work.
                    CmsisDapDevice::V2 { .. } => return Err(e.into()),
                },
            }
        }

        Err(CmsisDapError::NoReportSize)
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
    pub(super) fn read_swo_stream(&self, timeout: Duration) -> Result<Vec<u8>, CmsisDapError> {
        match self {
            CmsisDapDevice::V1 { .. } => Err(CmsisDapError::SwoModeNotAvailable),
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
                        Err(e) => Err(SendError::from(e).into()),
                    }
                }
                None => Err(CmsisDapError::SwoModeNotAvailable),
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
    pub fn from_byte(value: u8) -> Result<Self, SendError> {
        match value {
            0x00 => Ok(Status::DAPOk),
            0xFF => Ok(Status::DAPError),
            _ => Err(SendError::NonZeroOrFF),
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
    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize, SendError>;
}

pub(crate) trait Response: Sized {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self, SendError>;
}

pub(crate) fn send_command<Req: Request, Res: Response>(
    device: &mut CmsisDapDevice,
    request: Req,
) -> Result<Res, SendError> {
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
    let _ = device.write(&buffer[..size])?;
    trace_buffer("Transmit buffer", &buffer[..size]);

    // Read back response.
    let bytes_read = device.read(&mut buffer)?;
    let response_data = &buffer[..bytes_read];
    trace_buffer("Receive buffer", response_data);

    if response_data.is_empty() {
        return Err(SendError::NotEnoughData);
    }

    if response_data[0] == *Req::CATEGORY {
        Res::from_bytes(response_data, 1)
    } else {
        Err(SendError::InvalidDataFor(*Req::CATEGORY))
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
