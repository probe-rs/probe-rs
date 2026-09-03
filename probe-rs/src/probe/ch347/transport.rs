//! USB transport, command framing and the reply-stream guard.

use std::fmt::Debug;
use std::io;
use std::time::Duration;

use nusb::{
    Endpoint, Interface,
    transfer::{Bulk, In, Out},
};

use crate::probe::{
    DebugProbeError, ProbeError,
    usb_util::{BulkReadExt, BulkWriteExt},
};

use super::device::Ch347Device;

const TIMEOUT: Duration = Duration::from_millis(500);
/// Long enough for a reply already in the chip to arrive.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(20);
/// Every command and every reply starts with the command byte and a little-endian payload length.
pub(super) const HEADER_LEN: usize = 3;
/// The largest reply the chip sends, header included.
pub(super) const MAX_PACKET: usize = 512;

/// The command byte, the little-endian payload length, then the payload.
pub(super) fn frame(command: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.push(command);
    frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Ch347Error {
    #[error("unexpected reply {reply:02x?} to command {command:#04x}")]
    Reply { command: u8, reply: Vec<u8> },
    #[error("the reply stream is out of sync, reopen the probe")]
    OutOfSync,
    #[error("no vendor interface with bulk endpoints in both directions")]
    NoInterface,
    #[error("SWD batch exceeds the firmware's limits")]
    BatchTooLarge,
}

impl ProbeError for Ch347Error {}

/// The bulk pipe pair of the chip's vendor interface.
pub(crate) trait Transport: Debug + Send {
    fn write(&mut self, data: &[u8]) -> io::Result<()>;
    /// Reads one reply and returns its length.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
}

#[derive(Debug)]
pub(crate) struct UsbTransport {
    _interface: Interface,
    out: Endpoint<Bulk, Out>,
    inp: Endpoint<Bulk, In>,
}

impl UsbTransport {
    /// Claims the pipes and drops any reply a previous session left behind.
    pub(super) fn open(
        interface: Interface,
        out: Endpoint<Bulk, Out>,
        inp: Endpoint<Bulk, In>,
    ) -> Self {
        let mut transport = Self {
            _interface: interface,
            out,
            inp,
        };
        transport.drain();
        transport
    }

    /// Discards replies a previous session stopped waiting for, which the chip still holds.
    fn drain(&mut self) {
        let mut stale = [0; MAX_PACKET];
        while let Ok(len) = self.inp.read_bulk(&mut stale, DRAIN_TIMEOUT)
            && len > 0
        {
            tracing::debug!("discarded a stale reply: {:02x?}", &stale[..len]);
        }
    }
}

impl Transport for UsbTransport {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let written = self.out.write_bulk(data, TIMEOUT)?;
        if written == data.len() {
            Ok(())
        } else {
            Err(io::Error::other("short bulk write"))
        }
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inp.read_bulk(buf, TIMEOUT)
    }
}

impl Ch347Device {
    /// Marks the reply stream as unusable and returns the error for the reply that showed it.
    ///
    /// A reply that does not match its command means a late or lost reply is in the pipe;
    /// every later reply would be attributed to the wrong command, so the device refuses
    /// further commands until it is reopened.
    pub(super) fn desynced(&mut self, command: u8, reply: Vec<u8>) -> DebugProbeError {
        self.out_of_sync = true;
        Ch347Error::Reply { command, reply }.into()
    }

    /// Sends one command and returns its reply payload, which must be `reply_len` bytes.
    ///
    /// Reading exactly the expected length keeps a full-speed link from waiting for a
    /// short packet that never comes.
    pub(super) fn command(
        &mut self,
        command: u8,
        payload: &[u8],
        reply_len: usize,
    ) -> Result<Vec<u8>, DebugProbeError> {
        if self.out_of_sync {
            return Err(Ch347Error::OutOfSync.into());
        }
        let frame = frame(command, payload);
        tracing::trace!("> {frame:02x?}");
        if let Err(e) = self.transport.write(&frame) {
            self.out_of_sync = true;
            return Err(DebugProbeError::Usb(e));
        }

        let mut reply = vec![0; HEADER_LEN + reply_len];
        let len = match self.transport.read(&mut reply) {
            Ok(len) => len,
            Err(e) => {
                self.out_of_sync = true;
                return Err(DebugProbeError::Usb(e));
            }
        };
        reply.truncate(len);
        tracing::trace!("< {reply:02x?}");

        let framed = len == HEADER_LEN + reply_len
            && reply[0] == command
            && usize::from(u16::from_le_bytes([reply[1], reply[2]])) == reply_len;
        if !framed {
            return Err(self.desynced(command, reply));
        }
        reply.drain(..HEADER_LEN);
        Ok(reply)
    }

    /// The single status byte a command replies with.
    pub(super) fn command_status(
        &mut self,
        command: u8,
        payload: &[u8],
    ) -> Result<u8, DebugProbeError> {
        Ok(self.command(command, payload, 1)?[0])
    }
}
