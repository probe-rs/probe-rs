pub mod general;
pub mod swd;
pub mod swj;
pub mod swo;
pub mod transfer;

use crate::probe::cmsisdap::commands::general::info::PacketSizeCommand;
use crate::DebugProbeError;
use std::str::Utf8Error;
use std::time::Duration;

use log::log_enabled;

#[derive(Debug, thiserror::Error)]
pub enum CmsisDapError {
    #[error("Error handling CMSIS-DAP command {command_id:?}")]
    Send {
        command_id: CommandId,
        source: SendError,
    },
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
    #[error("USB Error reading SWO data.")]
    SwoReadError(#[source] rusb::Error),
    #[error("Could not determine a suitable packet size for this probe")]
    NoPacketSize,
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
    InvalidResponseStatus,
    #[error("Connecting to target failed, received: {0:x}")]
    ConnectResponseError(u8),
    #[error("Command ID in response (:#02x) does not match sent command ID")]
    CommandIdMismatch(u8),
    /// String in response is not valid UTF-8.
    ///
    /// Strings are required to be UTF-8 encoded by the
    /// CMSIS-DAP specification.
    #[error("String in response is not valid UTF-8.")]
    InvalidString(#[from] Utf8Error),
    #[error("Unexpected answer to command")]
    UnexpectedAnswer,
    #[error("Timeout in USB communication.")]
    Timeout,
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

    /// Set the packet size to use for this device.
    ///
    /// Sets either the HID report size for V1 devices,
    /// or the maximum bulk transfer size for V2 devices.
    pub(super) fn set_packet_size(&mut self, packet_size: usize) {
        log::debug!("Configuring probe to use packet size {}", packet_size);
        match self {
            CmsisDapDevice::V1 {
                ref mut report_size,
                ..
            } => {
                *report_size = packet_size;
            }
            CmsisDapDevice::V2 {
                ref mut max_packet_size,
                ..
            } => {
                *max_packet_size = packet_size;
            }
        }
    }

    /// Attempt to determine the correct packet size for this device.
    ///
    /// Tries to request the CMSIS-DAP maximum packet size, allowing several
    /// failures to accommodate some buggy probes which must receive a full
    /// packet worth of data before responding, but we don't know how much
    /// data that is before we get a response.
    ///
    /// The device is then configured to use the detected size, which is returned.
    pub(super) fn find_packet_size(&mut self) -> Result<usize, CmsisDapError> {
        for repeat in 0..16 {
            log::debug!("Attempt {} to find packet size", repeat + 1);
            match send_command(self, PacketSizeCommand {}) {
                Ok(size) => {
                    log::debug!("Success: packet size is {}", size);
                    self.set_packet_size(size as usize);
                    return Ok(size as usize);
                }

                // Ignore timeouts and retry.
                Err(CmsisDapError::Send {
                    source: SendError::Timeout,
                    ..
                }) => (),

                // Raise other errors.
                Err(e) => return Err(e.into()),
            }
        }

        // If we didn't return early, no sizes worked, report an error.
        Err(CmsisDapError::NoPacketSize)
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
                        Err(e) => Err(CmsisDapError::SwoReadError(e)),
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
            _ => Err(SendError::InvalidResponseStatus),
        }
    }
}

/// Command ID for CMSIS-DAP commands.
///
/// The command ID is always sent as the first byte for every command,
/// and also is the first byte of every response.
#[derive(Debug)]
pub enum CommandId {
    Info = 0x00,
    HostStatus = 0x01,
    Connect = 0x02,
    Disconnect = 0x03,
    WriteAbort = 0x08,
    Delay = 0x09,
    ResetTarget = 0x0A,
    SwjPins = 0x10,
    SwjClock = 0x11,
    SwjSequence = 0x12,
    SwdConfigure = 0x13,
    SwdSequence = 0x1D,
    SwoTransport = 0x17,
    SwoMode = 0x18,
    SwoBaudrate = 0x19,
    SwoControl = 0x1A,
    SwoStatus = 0x1B,
    SwoExtendedStatus = 0x1E,
    SwoData = 0x1C,
    JtagSequence = 0x14,
    JtagConfigure = 0x15,
    JtagIdcode = 0x16,
    TransferConfigure = 0x04,
    Transfer = 0x05,
    TransferBlock = 0x06,
    TransferAbort = 0x07,
    ExecuteCommands = 0x7F,
    QueueCommands = 0x7E,
    UartTransport = 0x1F,
    UartConfigure = 0x20,
    UartControl = 0x22,
    UartStatus = 0x23,
    UartTransfer = 0x21,
}

pub(crate) trait Request {
    const COMMAND_ID: CommandId;

    type Response;

    /// Convert the request to bytes, which can be sent to the probe.
    /// Returns the amount of bytes written to the buffer.
    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError>;

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError>;
}

pub(crate) fn send_command<Req: Request>(
    device: &mut CmsisDapDevice,
    request: Req,
) -> Result<Req::Response, CmsisDapError> {
    send_command_inner(device, request).map_err(|e| CmsisDapError::Send {
        command_id: Req::COMMAND_ID,
        source: e,
    })
}

fn send_command_inner<Req: Request>(
    device: &mut CmsisDapDevice,
    request: Req,
) -> Result<Req::Response, SendError> {
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
    buffer[1] = Req::COMMAND_ID as u8;
    let mut size = request.to_bytes(&mut buffer[2..])? + 2;

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

    if response_data[0] == Req::COMMAND_ID as u8 {
        request.from_bytes(&response_data[1..])
    } else {
        Err(SendError::CommandIdMismatch(response_data[0]))
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
