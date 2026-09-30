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

use super::board::Drive;
use super::device::Ch347Device;

pub(super) const TIMEOUT: Duration = Duration::from_millis(500);
/// A full round at the slowest clock takes under a second.
const ROUND_TIMEOUT: Duration = Duration::from_secs(2);
/// Long enough for a reply already in the chip to arrive.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(20);
/// Every command and every reply starts with the command byte and a little-endian payload length.
pub(super) const HEADER_LEN: usize = 3;
/// One high-speed USB packet, the most a single command or an SWD batch carries each way. The
/// replies of a JTAG round stream past it.
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
    #[error("the reset GPIO did not switch to {0:?}")]
    Reset(Drive),
    #[error("target reset needs a board with a reset GPIO; this is a generic CH347")]
    NoResetPin,
}

impl ProbeError for Ch347Error {}

/// The bulk pipe pair of the chip's vendor interface.
pub(crate) trait Transport: Debug + Send {
    fn write(&mut self, data: &[u8]) -> io::Result<()>;
    /// Reads one transfer of at most the buffer's length and returns its length.
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
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

    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.inp.read_bulk(buf, timeout)
    }
}

impl Ch347Device {
    /// Marks the reply stream unusable after a reply that does not match its command: a
    /// late or lost reply is in the pipe and every later reply would go to the wrong
    /// command, so the device must be reopened.
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
        self.send(&frame(command, payload))?;

        let mut reply = vec![0; HEADER_LEN + reply_len];
        let len = self.read_transfer(&mut reply, TIMEOUT)?;
        reply.truncate(len);
        tracing::trace!("< {reply:02x?}");

        let framed = len == HEADER_LEN + reply_len
            && reply[0] == command
            && usize::from(u16::from_le_bytes([reply[1], reply[2]])) == reply_len;
        if !framed {
            return Err(self.desynced(command, reply));
        }
        reply.drain(..HEADER_LEN);
        self.led_activity(command);
        Ok(reply)
    }

    /// Sends the concatenated `frames` of a JTAG round in one write and reads the replies of
    /// its reading commands, given as command byte and payload length in order.
    ///
    /// The replies come back as one stream, which may arrive in several transfers. Returns
    /// their payloads concatenated.
    pub(super) fn round(
        &mut self,
        frames: &[u8],
        replies: &[(u8, usize)],
    ) -> Result<Vec<u8>, DebugProbeError> {
        self.send(frames)?;

        let total = replies.iter().map(|(_, len)| HEADER_LEN + len).sum();
        let mut stream = vec![0; total];
        let mut received = 0;
        while received < total {
            received += self.read_transfer(&mut stream[received..], ROUND_TIMEOUT)?;
        }
        tracing::trace!("< {stream:02x?}");

        let mut payloads = Vec::with_capacity(total - HEADER_LEN * replies.len());
        let mut at = 0;
        for &(command, len) in replies {
            let header = &stream[at..at + HEADER_LEN];
            if header[0] != command
                || usize::from(u16::from_le_bytes([header[1], header[2]])) != len
            {
                let header = header.to_vec();
                return Err(self.desynced(command, header));
            }
            at += HEADER_LEN;
            payloads.extend_from_slice(&stream[at..at + len]);
            at += len;
        }
        Ok(payloads)
    }

    /// Writes `data` unless the reply stream is out of sync.
    fn send(&mut self, data: &[u8]) -> Result<(), DebugProbeError> {
        if self.out_of_sync {
            return Err(Ch347Error::OutOfSync.into());
        }
        tracing::trace!("> {data:02x?}");
        self.transport.write(data).map_err(|e| {
            self.out_of_sync = true;
            DebugProbeError::Usb(e)
        })
    }

    /// Reads one transfer and skips a zero-length one before it. A reply of whole packets may
    /// be ended by a zero-length packet, which the next read then gets first.
    fn read_transfer(
        &mut self,
        buf: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, DebugProbeError> {
        let read = match self.transport.read(buf, timeout) {
            Ok(0) => self.transport.read(buf, timeout),
            read => read,
        };
        read.map_err(|e| {
            self.out_of_sync = true;
            DebugProbeError::Usb(e)
        })
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

#[cfg(test)]
mod tests {
    use super::super::device::tests::{CH347F_1_20, scripted_transfers};
    use super::*;

    const D2: u8 = 0xD2;
    const D4: u8 = 0xD4;

    #[test]
    fn round_reads_the_reply_stream_across_transfers_and_a_bad_one_poisons_the_device() {
        // A D4 of three bytes and a D2 of two cycles.
        let mut frames = frame(D4, &[1, 2, 3]);
        frames.extend(frame(D2, &[0x20, 0x21, 0x20, 0x21, 0x20]));
        let mut stream = frame(D4, &[0xA, 0xB, 0xC]);
        stream.extend(frame(D2, &[1, 0]));
        let mut wrong = stream.clone();
        wrong[7] = 3;
        let cases = [
            // A split inside the second header.
            (vec![stream[..7].to_vec(), stream[7..].to_vec()], true),
            // The D2 reply claims three bytes.
            (vec![wrong], false),
            // A timeout after the first transfer.
            (vec![stream[..4].to_vec(), vec![]], false),
        ];
        let replies = [(D4, 3), (D2, 2)];
        for (transfers, good) in cases {
            let exchanges = vec![
                (frames.clone(), transfers),
                (frames.clone(), vec![stream.clone()]),
            ];
            let (mut dev, script) = scripted_transfers(CH347F_1_20, None, exchanges);
            let payloads = dev.round(&frames, &replies).ok();
            assert_eq!(payloads, good.then(|| vec![0xA, 0xB, 0xC, 1, 0]));
            // After a bad stream nothing more goes out.
            assert_eq!(dev.round(&frames, &replies).is_ok(), good);
            assert_eq!(script.finished(), good);
        }
    }
}
