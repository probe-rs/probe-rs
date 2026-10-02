#[cfg(test)]
pub(crate) mod fake;
pub mod general;
pub mod jtag;
pub mod swd;
pub mod swj;
pub mod swo;
pub mod transfer;

use crate::probe::cmsisdap::commands::general::info::{PacketCountCommand, PacketSizeCommand};
use crate::probe::usb_util::{BulkReadExt, BulkWriteExt};
use crate::probe::{ProbeError, WireProtocol};
use nusb::{
    Endpoint,
    transfer::{Bulk, In, Out},
};
use std::io::ErrorKind;
use std::str::Utf8Error;
use std::time::Duration;

use self::general::host_status::HostStatusRequest;
use self::swj::clock::SWJClockRequest;
use self::transfer::TransferAbortRequest;

pub(crate) const DEFAULT_USB_TIMEOUT: Duration = Duration::from_millis(1000);

/// Rounds spent bringing replies back into step before giving up on the probe.
const MAX_RESYNC_ROUNDS: usize = 32;

/// How long to wait for a reply the probe was prompted to give up.
const RESYNC_IDLE: Duration = Duration::from_millis(5);

/// The USB timeout while resynchronising. A probe on a local USB link answers within it. A slower
/// one shows itself in a silent round, and gets the full USB timeout from then on.
const RESYNC_TIMEOUT: Duration = Duration::from_millis(50);

#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum CmsisDapError {
    /// Error handling CMSIS-DAP command {command_id:?}.
    Send {
        command_id: CommandId,
        source: SendError,
    },

    /// CMSIS-DAP responded with an error.
    ErrorResponse(#[source] RequestError),

    /// Too much data provided for SWJ Sequence command.
    TooMuchData,

    /// Requested SWO baud rate could not be configured.
    SwoBaudrateNotConfigured,

    /// Probe reported an error while streaming SWO.
    SwoTraceStreamError,

    /// Requested SWO mode is not available on this probe.
    SwoModeNotAvailable,

    /// USB Error reading SWO data.
    SwoReadError(#[source] std::io::Error),

    /// Could not determine a suitable packet size for this probe.
    NoPacketSize,

    /// Invalid IDCODE detected.
    InvalidIdCode,

    /// Error scanning IR lengths.
    InvalidIR,

    /// The firmware on the probe is outdated, and not supported by probe-rs. The minimum supported firmware version is {0}.
    ProbeFirmwareOutdated(&'static str),
}

impl ProbeError for CmsisDapError {}

#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum SendError {
    /// Error in the USB HID access.
    #[cfg(feature = "cmsisdap_v1")]
    HidApi(#[from] hidapi::HidError),

    /// Error in the USB access.
    UsbError(std::io::Error),

    /// Not enough data in response from probe.
    NotEnoughData,

    /// Status can only be 0x00 or 0xFF
    InvalidResponseStatus,

    /// Connecting to target failed, received: {0:x}
    ConnectResponseError(u8),

    /// Command ID in response ({0:#02x}) does not match sent command ID ({1:?} - {*_1 as u8:#02x}).
    CommandIdMismatch(u8, CommandId),

    /// String in response is not valid UTF-8.
    ///
    /// Strings are required to be UTF-8 encoded by the
    /// CMSIS-DAP specification.
    #[ignore_extra_doc_attributes]
    InvalidString(#[from] Utf8Error),

    /// Unexpected answer to command.
    UnexpectedAnswer,

    /// Timeout in USB communication.
    Timeout,
}

impl From<std::io::Error> for SendError {
    fn from(error: std::io::Error) -> Self {
        match error.kind() {
            ErrorKind::TimedOut => SendError::Timeout,
            _ => SendError::UsbError(error),
        }
    }
}

#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum RequestError {
    /// Failed setting the SWJ Clock on probe with the following request: {request:?}
    SWJClock { request: SWJClockRequest },

    /// Failed to configure the SWD options on probe with the following request: {request:?}
    SwdConfigure {
        request: swd::configure::ConfigureRequest,
    },

    /// Failed to configure the JTAG options on probe with the following request: {request:?}
    JtagConfigure {
        request: jtag::configure::ConfigureRequest,
    },

    /// Failed to configure the transfer options on probe with the following request: {request:?}
    TransferConfigure {
        request: transfer::configure::ConfigureRequest,
    },

    /// Failed to send the SWD sequence to the probe with the following request: {request:?}
    SwjSequence {
        request: swj::sequence::SequenceRequest,
    },

    /// Failed to send the JTAG sequence to the probe with the following request: {request:?}
    JtagSequence {
        request: jtag::sequence::SequenceRequest,
    },

    /// The JTAG `{name}` scan chain is either too long or otherwise broken. Expected next bit to be {expected_bit}
    BrokenScanChain {
        name: &'static str,
        expected_bit: u8,
    },

    /// The JTAG `{name}` scan chain is empty
    EmptyScanChain { name: &'static str },

    /// Could not set {transport:?} as the SWO transport
    SwoTransport { transport: swo::TransportRequest },

    /// Could not set {mode:?} as the SWO mode
    SwoMode { mode: swo::ModeRequest },

    /// Could not execute SWO control command {command:?}
    SwoControl { command: swo::ControlRequest },

    /// {protocol:?} initialization failed
    InitFailed { protocol: Option<WireProtocol> },

    /// Setting the host status on the debug probe failed with request {request:?}
    HostStatus { request: HostStatusRequest },
}

pub enum CmsisDapDevice {
    /// CMSIS-DAP v1 over HID.
    /// Stores a HID device handle and maximum HID report size.
    #[cfg(feature = "cmsisdap_v1")]
    V1 {
        handle: hidapi::HidDevice,
        report_size: usize,
        usb_timeout: Duration,
    },

    /// CMSIS-DAP v2 over WinUSB/Bulk.
    /// Stores the usb interface handle, persistent bulk out/in endpoints, the
    /// maximum DAP packet size, and an optional persistent SWO streaming
    /// endpoint.
    ///
    /// The endpoints are claimed once when the device is opened and reused for
    /// every transfer, rather than being re-claimed per transfer. This both
    /// removes per-transfer setup/teardown cost and provides a stable place to
    /// keep multiple transfers in flight (see FAST_USB.md).
    V2 {
        handle: nusb::Interface,
        out_ep: Endpoint<Bulk, Out>,
        in_ep: Endpoint<Bulk, In>,
        max_packet_size: usize,
        swo_ep: Option<Endpoint<Bulk, In>>,
        usb_timeout: Duration,
    },

    /// A scripted probe for host tests.
    #[cfg(test)]
    Fake(std::sync::Arc<std::sync::Mutex<fake::FakeProbe>>),
}

impl CmsisDapDevice {
    fn usb_timeout(&self) -> Duration {
        match self {
            #[cfg(feature = "cmsisdap_v1")]
            Self::V1 { usb_timeout, .. } => *usb_timeout,
            Self::V2 { usb_timeout, .. } => *usb_timeout,
            #[cfg(test)]
            Self::Fake(probe) => probe.lock().unwrap().usb_timeout,
        }
    }

    fn set_usb_timeout(&mut self, timeout: Duration) {
        match self {
            #[cfg(feature = "cmsisdap_v1")]
            Self::V1 { usb_timeout, .. } => *usb_timeout = timeout,
            Self::V2 { usb_timeout, .. } => *usb_timeout = timeout,
            #[cfg(test)]
            Self::Fake(probe) => probe.lock().unwrap().usb_timeout = timeout,
        }
    }

    /// Read from the probe into `buf`, returning the number of bytes read on success.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, SendError> {
        match self {
            #[cfg(feature = "cmsisdap_v1")]
            CmsisDapDevice::V1 {
                handle,
                usb_timeout,
                ..
            } => {
                match handle.read_timeout(buf, usb_timeout.as_millis() as i32)? {
                    // Timeout is not indicated by error, but by returning 0 read bytes
                    0 => Err(SendError::Timeout),
                    n => Ok(n),
                }
            }
            CmsisDapDevice::V2 {
                in_ep, usb_timeout, ..
            } => Ok(in_ep.read_bulk(buf, *usb_timeout)?),
            #[cfg(test)]
            CmsisDapDevice::Fake(probe) => {
                let reply = probe.lock().unwrap().read().ok_or(SendError::Timeout)?;
                buf[..reply.len()].copy_from_slice(&reply);
                Ok(reply.len())
            }
        }
    }

    /// Write `buf` to the probe, returning the number of bytes written on success.
    fn write(&mut self, buf: &[u8]) -> Result<usize, SendError> {
        match self {
            #[cfg(feature = "cmsisdap_v1")]
            CmsisDapDevice::V1 { handle, .. } => Ok(handle.write(buf)?),
            CmsisDapDevice::V2 {
                out_ep,
                usb_timeout,
                ..
            } => {
                // Skip first byte as it's set to 0 for HID transfers
                Ok(out_ep.write_bulk(&buf[1..], *usb_timeout)?)
            }
            #[cfg(test)]
            CmsisDapDevice::Fake(probe) => {
                probe.lock().unwrap().write(&buf[1..]);
                Ok(buf.len() - 1)
            }
        }
    }

    /// Drain any pending data from the probe, ensuring future responses are
    /// synchronised to requests. Swallows any errors, which are expected if
    /// there is no pending data to read.
    pub(super) fn drain(&mut self) {
        self.drain_idle_for(Duration::from_millis(1));
    }

    /// Drain as [`CmsisDapDevice::drain`], treating the queue as empty only once the probe has
    /// produced nothing for `idle`.
    ///
    /// A probe working through a backlog hands its replies over one at a time and not always
    /// promptly, so the short window that suffices for an ordinary open sees an empty pipe and
    /// leaves the rest of the backlog in place.
    ///
    /// Returns whether anything was read.
    pub(super) fn drain_idle_for(&mut self, idle: Duration) -> bool {
        tracing::debug!("Draining probe of any pending data.");

        let mut drained = false;
        match self {
            #[cfg(feature = "cmsisdap_v1")]
            CmsisDapDevice::V1 {
                handle,
                report_size,
                ..
            } => loop {
                let mut discard = vec![0u8; *report_size + 1];
                match handle.read_timeout(&mut discard, idle.as_millis().max(1) as i32) {
                    Ok(n) if n != 0 => drained = true,
                    _ => break,
                }
            },

            CmsisDapDevice::V2 {
                in_ep,
                max_packet_size,
                ..
            } => {
                let timeout = idle;
                let mut discard = vec![0u8; *max_packet_size];
                loop {
                    match in_ep.read_bulk(&mut discard, timeout) {
                        Ok(n) if n != 0 => drained = true,
                        _ => break,
                    }
                }
            }

            #[cfg(test)]
            CmsisDapDevice::Fake(probe) => drained = probe.lock().unwrap().drain(),
        }
        drained
    }

    /// Set the packet size to use for this device.
    ///
    /// Sets either the HID report size for V1 devices,
    /// or the maximum bulk transfer size for V2 devices.
    pub(super) fn set_packet_size(&mut self, packet_size: usize) {
        tracing::debug!("Configuring probe to use packet size {}", packet_size);
        match self {
            #[cfg(feature = "cmsisdap_v1")]
            CmsisDapDevice::V1 { report_size, .. } => {
                *report_size = packet_size;
            }
            CmsisDapDevice::V2 {
                max_packet_size, ..
            } => {
                *max_packet_size = packet_size;
            }
            #[cfg(test)]
            CmsisDapDevice::Fake(probe) => probe.lock().unwrap().max_packet_size = packet_size,
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
        // Use a short USB timeout when determining packet size as otherwise we wait
        // several seconds each time for enough data to accumulate.
        let old_timeout = self.usb_timeout();
        self.set_usb_timeout(Duration::from_millis(50));
        for repeat in 0..16 {
            tracing::debug!("Attempt {} to find packet size", repeat + 1);
            match send_command_once(self, &PacketSizeCommand {}) {
                Ok(size) => {
                    tracing::debug!("Success: packet size is {}", size);
                    self.set_usb_timeout(old_timeout);
                    self.set_packet_size(size as usize);
                    return Ok(size as usize);
                }

                // Ignore timeouts and retry.
                Err(CmsisDapError::Send {
                    source: SendError::Timeout,
                    ..
                }) => (),

                // A reply to a command from before this device was opened. It has now been read
                // and the rest of the queue discarded with it, so the next attempt gets its own.
                Err(CmsisDapError::Send {
                    source: SendError::CommandIdMismatch(..),
                    ..
                }) => (),

                // Raise other errors.
                Err(e) => return Err(e),
            }
        }

        // If we didn't return early, no sizes worked, report an error.
        Err(CmsisDapError::NoPacketSize)
    }

    /// Bring requests and replies back into step.
    ///
    /// A probe interrupted mid-transfer answers the next command with the previous one's reply,
    /// and goes on doing so across a close and reopen. Draining does not clear it: nothing is
    /// waiting on the endpoint, so a drain reads nothing and the next command is answered late all
    /// the same. Ordinary commands do not clear it either, however many are sent.
    ///
    /// What does clear it, measured a reply at a time, is `DAP_TransferAbort`, which the probe
    /// answers with nothing at all. Some firmware, debugprobe among it, runs the abort in order
    /// behind the stalled transfer instead, and answers it with `0xFF`.
    ///
    /// Whether the stream is level has to be asked with two different commands. A repeated one
    /// cannot tell its own reply from the previous copy's. The two are the `DAP_Info` packet count
    /// and packet size: they share a command ID, but their replies differ in length, so neither
    /// passes for the other, and neither passes for the abort's `0xFF`. The order alternates
    /// between rounds, so that late replies to the previous round's pair cannot pass.
    pub(super) fn resynchronise(&mut self) {
        let usb_timeout = self.usb_timeout();
        self.set_usb_timeout(RESYNC_TIMEOUT);
        let in_step = self.resynchronise_rounds(usb_timeout);
        self.set_usb_timeout(usb_timeout);

        if !in_step {
            tracing::warn!("Could not bring the probe's replies back into step with its requests.");
        }
    }

    fn resynchronise_rounds(&mut self, patience: Duration) -> bool {
        for round in 0..MAX_RESYNC_ROUNDS {
            // Unconditionally, and before asking anything: a probe that owes a reply does not give
            // it up for a command that queues another one behind it, so the question cannot be
            // asked until this has been done at least once.
            let _ = send_request(self, &TransferAbortRequest);
            let drained = self.drain_idle_for(RESYNC_IDLE);

            let mut checks = [Self::check_packet_count, Self::check_packet_size];
            if round % 2 == 1 {
                checks.reverse();
            }
            let results = checks.map(|check| check(self));

            if results.iter().all(Result::is_ok) {
                if round > 0 {
                    tracing::debug!("Probe back in step after {round} rounds");
                }
                return true;
            }

            // A probe that runs a long transfer is silent for a while. One that stays silent, or
            // is unplugged, does not come back by asking it again.
            let replied = results.iter().any(|result| {
                result.as_ref().is_ok() || result.as_ref().is_err_and(reply_is_not_ours)
            });
            if !drained && !replied {
                if !self.drain_idle_for(patience) {
                    break;
                }
                self.set_usb_timeout(patience);
            }
        }

        false
    }

    fn check_packet_count(&mut self) -> Result<(), CmsisDapError> {
        send_command_once(self, &PacketCountCommand {}).map(drop)
    }

    fn check_packet_size(&mut self) -> Result<(), CmsisDapError> {
        send_command_once(self, &PacketSizeCommand {}).map(drop)
    }

    /// Check if SWO streaming is supported by this device.
    pub(super) fn swo_streaming_supported(&self) -> bool {
        match self {
            #[cfg(feature = "cmsisdap_v1")]
            CmsisDapDevice::V1 { .. } => false,
            CmsisDapDevice::V2 { swo_ep, .. } => swo_ep.is_some(),
            #[cfg(test)]
            CmsisDapDevice::Fake(_) => false,
        }
    }

    /// Read from the SWO streaming endpoint.
    ///
    /// Returns SWOModeNotAvailable if this device does not support SWO streaming.
    ///
    /// On timeout, returns a zero-length buffer.
    pub(super) fn read_swo_stream(&mut self, timeout: Duration) -> Result<Vec<u8>, CmsisDapError> {
        match self {
            #[cfg(feature = "cmsisdap_v1")]
            CmsisDapDevice::V1 { .. } => Err(CmsisDapError::SwoModeNotAvailable),
            CmsisDapDevice::V2 { swo_ep, .. } => match swo_ep {
                Some(ep) => {
                    let mut buf = vec![0u8; ep.max_packet_size()];
                    match ep.read_bulk(&mut buf, timeout) {
                        Ok(n) => {
                            buf.truncate(n);
                            Ok(buf)
                        }
                        Err(e) if e.kind() == ErrorKind::TimedOut => {
                            buf.clear();
                            Ok(buf)
                        }
                        Err(e) => Err(CmsisDapError::SwoReadError(e)),
                    }
                }
                None => Err(CmsisDapError::SwoModeNotAvailable),
            },
            #[cfg(test)]
            CmsisDapDevice::Fake(_) => Err(CmsisDapError::SwoModeNotAvailable),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Status {
    DapOk = 0x00,
    DapError = 0xFF,
}

impl Status {
    pub fn from_byte(value: u8) -> Result<Self, SendError> {
        match value {
            0x00 => Ok(Status::DapOk),
            0xFF => Ok(Status::DapError),
            _ => Err(SendError::InvalidResponseStatus),
        }
    }
}

/// Command ID for CMSIS-DAP commands.
///
/// The command ID is always sent as the first byte for every command,
/// and also is the first byte of every response.
#[derive(Debug, Clone, Copy)]
#[expect(unused)]
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

    /// Parse the response to this request from received bytes.
    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError>;
}

/// Send a request and take its reply. After a failure that can leave a reply behind, bring the
/// replies back into step, so that the next command reads its own.
pub(crate) fn send_command<Req: Request>(
    device: &mut CmsisDapDevice,
    request: &Req,
) -> Result<Req::Response, CmsisDapError> {
    let response = send_command_once(device, request);
    if let Err(error) = &response
        && may_be_out_of_step(error)
    {
        device.resynchronise();
    }
    response
}

/// Send a request and take its reply, without the recovery of [`send_command`].
pub(crate) fn send_command_once<Req: Request>(
    device: &mut CmsisDapDevice,
    request: &Req,
) -> Result<Req::Response, CmsisDapError> {
    send_command_inner(device, request).map_err(|e| CmsisDapError::Send {
        command_id: Req::COMMAND_ID,
        source: e,
    })
}

/// Whether the probe can still owe a reply after this error, or has already answered an earlier
/// command in place of this one.
///
/// A late reply arrives after the 1 ms drain that follows the error, so the drain alone does not
/// take it off the device.
pub(crate) fn may_be_out_of_step(error: &CmsisDapError) -> bool {
    match error {
        CmsisDapError::Send { source, .. } => match source {
            SendError::Timeout | SendError::NotEnoughData | SendError::UnexpectedAnswer => true,
            SendError::CommandIdMismatch(id, _) => *id != 0xFF,
            _ => false,
        },
        _ => false,
    }
}

/// Whether an error says the probe answered with something that was not a reply to what was asked.
///
/// A timeout or a USB failure says the probe is gone or silent, which asking again does not fix.
/// These say a reply arrived and made no sense as an answer to the question, which is what a
/// backlog from an earlier session looks like.
pub(crate) fn reply_is_not_ours(error: &CmsisDapError) -> bool {
    matches!(
        error,
        CmsisDapError::Send {
            source: SendError::CommandIdMismatch(..)
                | SendError::UnexpectedAnswer
                | SendError::NotEnoughData,
            ..
        }
    )
}

/// Treats the `0xFF` reply of a probe that does not implement a command as success.
///
/// That reply still answers this command and not an earlier one, so the stream is in step.
pub(crate) fn ignore_unknown_command<T>(
    result: Result<T, CmsisDapError>,
) -> Result<(), CmsisDapError> {
    match result {
        Ok(_)
        | Err(CmsisDapError::Send {
            source: SendError::CommandIdMismatch(0xFF, _),
            ..
        }) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Send a request without waiting for its reply.
///
/// The probe owes a reply for every request it accepts, and they come back in order. A caller
/// that sends more than one before reading must take the same number of replies, in the same
/// order, or the next command reads someone else's answer.
pub(crate) fn send_request<Req: Request>(
    device: &mut CmsisDapDevice,
    request: &Req,
) -> Result<(), CmsisDapError> {
    let mut buffer = vec![0; packet_buffer_len(device)];
    send_request_inner(device, request, &mut buffer).map_err(|e| CmsisDapError::Send {
        command_id: Req::COMMAND_ID,
        source: e,
    })
}

/// Take the reply to a request already sent by [`send_request`].
pub(crate) fn receive_response<Req: Request>(
    device: &mut CmsisDapDevice,
    request: &Req,
) -> Result<Req::Response, CmsisDapError> {
    let mut buffer = vec![0; packet_buffer_len(device)];
    receive_response_inner(device, request, &mut buffer).map_err(|e| CmsisDapError::Send {
        command_id: Req::COMMAND_ID,
        source: e,
    })
}

/// Size a buffer for the largest packet the device can carry, plus the HID report id.
fn packet_buffer_len(device: &CmsisDapDevice) -> usize {
    match device {
        #[cfg(feature = "cmsisdap_v1")]
        CmsisDapDevice::V1 { report_size, .. } => *report_size + 1,
        CmsisDapDevice::V2 {
            max_packet_size, ..
        } => *max_packet_size + 1,
        #[cfg(test)]
        CmsisDapDevice::Fake(probe) => probe.lock().unwrap().max_packet_size + 1,
    }
}

/// `buffer` must be zeroed past the request. A v1 device is sent a whole report, so whatever is
/// left in the tail goes on the wire.
fn send_request_inner<Req: Request>(
    device: &mut CmsisDapDevice,
    request: &Req,
    buffer: &mut [u8],
) -> Result<(), SendError> {
    // Leave byte 0 as the HID report, and write the command and request to the buffer.
    buffer[1] = Req::COMMAND_ID as u8;
    #[cfg_attr(not(feature = "cmsisdap_v1"), allow(unused_mut))]
    let mut size = request.to_bytes(&mut buffer[2..])? + 2;

    // For HID devices we must write a full report every time,
    // so set the transfer size to the report size, plus one
    // byte for the HID report ID. On v2 devices, we just
    // write the exact required size every time.
    #[cfg(feature = "cmsisdap_v1")]
    if let CmsisDapDevice::V1 { report_size, .. } = device {
        size = *report_size + 1;
    }

    let _ = device.write(&buffer[..size])?;
    trace_buffer("Transmit buffer", &buffer[..size]);

    Ok(())
}

fn receive_response_inner<Req: Request>(
    device: &mut CmsisDapDevice,
    request: &Req,
    buffer: &mut [u8],
) -> Result<Req::Response, SendError> {
    let bytes_read = device.read(buffer)?;
    let response_data = &buffer[..bytes_read];
    trace_buffer("Receive buffer", response_data);

    if response_data.is_empty() {
        return Err(SendError::NotEnoughData);
    }

    if response_data[0] == Req::COMMAND_ID as u8 {
        request.parse_response(&response_data[1..])
    } else {
        Err(SendError::CommandIdMismatch(
            response_data[0],
            Req::COMMAND_ID,
        ))
    }
}

fn send_command_inner<Req: Request>(
    device: &mut CmsisDapDevice,
    request: &Req,
) -> Result<Req::Response, SendError> {
    let mut buffer = vec![0; packet_buffer_len(device)];

    send_request_inner(device, request, &mut buffer)?;

    // Once the request is out the probe owes a reply, so a failure here has to take it off the
    // device before returning. Left there, the next command reads this reply instead of its own
    // and rejects it as the wrong command, and so does every command after that for as long as
    // the device stays open.
    let response = receive_response_inner(device, request, &mut buffer);
    if response.is_err() {
        device.drain();
    }

    response
}

/// Trace log a buffer, including only the first trailing zero.
///
/// This is useful for the CMSIS-DAP USB buffers, which often contain many trailing
/// zeros required for the various USB APIs, but make the trace output very long and
/// difficult to read.
fn trace_buffer(name: &str, buf: &[u8]) {
    if tracing::enabled!(tracing::Level::TRACE) {
        let len = buf.len();
        let cut = len + 1 - buf.iter().rev().position(|&x| x != 0).unwrap_or(len);
        let end = cut.clamp(1, len);
        tracing::trace!("{}: {:02X?}...", name, &buf[..end]);
    }
}
