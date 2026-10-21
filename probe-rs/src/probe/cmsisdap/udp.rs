//! CMSIS-DAP over UDP.
//!
//! Every command travels in one datagram behind a 4 byte header: the magic byte `0xDA`, a byte of
//! flags, slot and version, and a big endian 16 bit sequence number. The version (bits 0-3) is 1;
//! the slot (bits 4-6) picks one of up to eight probes at the address; flag `0x80` (SYNC) starts
//! a new sequence at the request that carries it. The reply has the same header without SYNC,
//! followed by the response. Payloads are those of CMSIS-DAP v2 bulk transfers.
//!
//! The probe runs requests in sequence order and keeps the responses to its last few, so several
//! requests can be in flight and a lost one is resent go-back-N: a request that already ran is
//! answered from the stored response, not run again.
//!
//! The probe is selected with a placeholder VID:PID and its address, and optionally its slot, as
//! the serial number: for example `--probe 0000:0002:10.42.0.50:1235/3`. The slot defaults to 0.

use std::collections::VecDeque;
use std::io::{self, ErrorKind};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

/// Placeholder vendor id that routes a selector to the UDP transport.
pub(crate) const UDP_VID: u16 = 0x0000;

/// Placeholder product id that routes a selector to the UDP transport.
pub(crate) const UDP_PID: u16 = 0x0002;

const DEFAULT_PORT: u16 = 1235;

const MAGIC: u8 = 0xDA;
const VERSION: u8 = 0x01;
const SYNC: u8 = 0x80;
const SLOTS: u8 = 8;
const HEADER_LEN: usize = 4;

/// Shortest wait for a response, as for the TCP transport.
const MIN_READ_TIMEOUT: Duration = Duration::from_millis(150);

/// How late the oldest reply may be before everything outstanding is sent again.
const RETRANSMIT_AFTER: Duration = Duration::from_millis(20);

/// A CMSIS-DAP probe reached over UDP.
pub struct UdpDevice {
    socket: UdpSocket,
    address: String,
    /// The version and slot bits of every request's and reply's second byte.
    version: u8,
    /// The sequence number of the next request.
    sequence: u16,
    /// Whether the probe has been told where this host's sequence starts.
    synced: bool,
    /// Requests sent and not yet answered, oldest first.
    outstanding: VecDeque<(u16, Vec<u8>)>,
    retransmissions: u64,
}

impl UdpDevice {
    /// Open a socket towards `address`, given as `host`, `host:port` or `[ipv6]:port`, each
    /// optionally followed by `/slot`.
    pub fn connect(address: &str) -> io::Result<Self> {
        let (host, slot) = match address.rsplit_once('/') {
            Some((host, slot)) => match slot.parse::<u8>() {
                Ok(slot) if slot < SLOTS => (host, slot),
                _ => {
                    return Err(io::Error::new(
                        ErrorKind::InvalidInput,
                        format!("UDP probe slot {slot:?} is not 0 to {}", SLOTS - 1),
                    ));
                }
            },
            None => (address, 0),
        };
        let peer = resolve(host)?
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "address resolved to nothing"))?;
        let local: SocketAddr = if peer.is_ipv4() {
            "0.0.0.0:0".parse().unwrap()
        } else {
            "[::]:0".parse().unwrap()
        };
        let socket = UdpSocket::bind(local)?;
        socket.connect(peer)?;
        Ok(Self {
            socket,
            address: address.to_string(),
            version: VERSION | slot << 4,
            sequence: rand_sequence(),
            synced: false,
            outstanding: VecDeque::new(),
            retransmissions: 0,
        })
    }

    /// The address the device was opened with.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Send one command.
    pub fn write(&mut self, payload: &[u8]) -> io::Result<usize> {
        let mut datagram = Vec::with_capacity(HEADER_LEN + payload.len());
        let flags = if self.synced {
            self.version
        } else {
            self.version | SYNC
        };
        datagram.extend_from_slice(&[MAGIC, flags]);
        datagram.extend_from_slice(&self.sequence.to_be_bytes());
        datagram.extend_from_slice(payload);
        self.socket.send(&datagram)?;
        self.outstanding.push_back((self.sequence, datagram));
        self.sequence = self.sequence.wrapping_add(1);
        self.synced = true;
        Ok(payload.len())
    }

    /// Receive the response to the oldest outstanding command into `buf`, returning its length.
    ///
    /// Resends everything outstanding whenever the reply is [`RETRANSMIT_AFTER`] late.
    pub fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let Some(&(expected, _)) = self.outstanding.front() else {
            return self.discard_for(timeout);
        };
        let deadline = Instant::now() + timeout.max(MIN_READ_TIMEOUT);
        let mut resend_at = Instant::now() + RETRANSMIT_AFTER;
        let mut datagram = [0u8; 2048];

        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(ErrorKind::TimedOut.into());
            }
            if now >= resend_at {
                tracing::debug!(
                    "No reply to UDP request {expected} yet; sending {} again",
                    self.outstanding.len()
                );
                for (_, request) in &self.outstanding {
                    self.socket.send(request)?;
                    self.retransmissions += 1;
                }
                resend_at = now + RETRANSMIT_AFTER;
            }
            self.socket.set_read_timeout(Some(
                resend_at
                    .min(deadline)
                    .saturating_duration_since(now)
                    .max(Duration::from_micros(1)),
            ))?;

            let length = match self.socket.recv(&mut datagram) {
                Ok(length) => length,
                Err(error) if is_timeout(&error) => continue,
                // ICMP port unreachable for an earlier datagram.
                Err(error) if error.kind() == ErrorKind::ConnectionRefused => continue,
                Err(error) => return Err(error),
            };
            if length < HEADER_LEN
                || datagram[0] != MAGIC
                || datagram[1] != self.version
                || u16::from_be_bytes([datagram[2], datagram[3]]) != expected
            {
                // Not ours, a duplicate, or sent after a lost request.
                continue;
            }

            let payload = &datagram[HEADER_LEN..length];
            if payload.len() > buf.len() {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "CMSIS-DAP UDP response longer than the packet size",
                ));
            }
            buf[..payload.len()].copy_from_slice(payload);
            self.outstanding.pop_front();
            return Ok(payload.len());
        }
    }

    /// Whether a reply is still owed.
    pub fn has_outstanding(&self) -> bool {
        !self.outstanding.is_empty()
    }

    /// Forget every outstanding command, and start a new sequence with the next one.
    pub fn forget_outstanding(&mut self) {
        self.outstanding.clear();
        self.synced = false;
    }

    /// How many datagrams have been sent again.
    #[cfg(test)]
    pub fn retransmissions(&self) -> u64 {
        self.retransmissions
    }

    fn discard_for(&mut self, timeout: Duration) -> io::Result<usize> {
        self.socket
            .set_read_timeout(Some(timeout.max(Duration::from_micros(1))))?;
        let mut datagram = [0u8; 2048];
        match self.socket.recv(&mut datagram) {
            Ok(_) => Err(io::Error::new(
                ErrorKind::InvalidData,
                "CMSIS-DAP UDP response to no outstanding command",
            )),
            Err(error) if is_timeout(&error) => Err(ErrorKind::TimedOut.into()),
            Err(error) => Err(error),
        }
    }
}

/// A socket read timeout is `WouldBlock` on Unix and `TimedOut` on Windows.
fn is_timeout(error: &io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

/// A starting sequence number that differs between sessions.
fn rand_sequence() -> u16 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    (nanos ^ std::process::id()) as u16
}

/// Resolve `host`, `host:port` or `[ipv6]:port`, defaulting the port to [`DEFAULT_PORT`].
fn resolve(address: &str) -> io::Result<Vec<SocketAddr>> {
    if let Ok(addresses) = address.to_socket_addrs() {
        return Ok(addresses.collect());
    }
    let host = address.trim_start_matches('[').trim_end_matches(']');
    Ok((host, DEFAULT_PORT).to_socket_addrs()?.collect())
}

#[cfg(test)]
mod test {
    use super::*;
    use std::thread;

    /// A loopback probe: `serve` gets each request's sequence number, flags and payload and
    /// returns the datagrams to send back, if any.
    fn probe(
        mut serve: impl FnMut(u16, u8, &[u8]) -> Vec<(u16, Vec<u8>)> + Send + 'static,
        requests: usize,
        slot: &str,
    ) -> (UdpDevice, thread::JoinHandle<()>) {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap().to_string();
        let handle = thread::spawn(move || {
            let mut datagram = [0u8; 2048];
            for _ in 0..requests {
                let (length, peer) = socket.recv_from(&mut datagram).unwrap();
                let sequence = u16::from_be_bytes([datagram[2], datagram[3]]);
                for (reply_sequence, payload) in serve(sequence, datagram[1], &datagram[4..length])
                {
                    let mut reply = vec![MAGIC, datagram[1] & !SYNC];
                    reply.extend_from_slice(&reply_sequence.to_be_bytes());
                    reply.extend_from_slice(&payload);
                    socket.send_to(&reply, peer).unwrap();
                }
            }
        });
        (
            UdpDevice::connect(&format!("{address}{slot}")).unwrap(),
            handle,
        )
    }

    #[test]
    fn the_first_request_starts_a_sequence_and_replies_come_in_order() {
        let (mut device, handle) = probe(
            |sequence, flags, payload| vec![(sequence, vec![payload[0], flags])],
            3,
            "/3",
        );
        for command in [0x00, 0x05, 0x06] {
            device.write(&[command]).unwrap();
        }
        let mut buf = [0u8; 64];
        let timeout = Duration::from_secs(1);
        assert_eq!(device.read(&mut buf, timeout).unwrap(), 2);
        assert_eq!(buf[..2], [0x00, SYNC | 3 << 4 | VERSION]);
        assert_eq!(device.read(&mut buf, timeout).unwrap(), 2);
        assert_eq!(buf[..2], [0x05, 3 << 4 | VERSION]);
        assert_eq!(device.read(&mut buf, timeout).unwrap(), 2);
        assert_eq!(buf[..2], [0x06, 3 << 4 | VERSION]);
        handle.join().unwrap();
    }

    #[test]
    fn a_lost_reply_is_asked_for_again_and_duplicates_are_ignored() {
        let mut first = true;
        let (mut device, handle) = probe(
            move |sequence, _, payload| {
                if first {
                    // The reply is lost, and a stale one from long ago arrives instead.
                    first = false;
                    vec![(sequence.wrapping_sub(7), vec![0xEE])]
                } else {
                    // The retransmission is answered twice, as a replay can be.
                    vec![(sequence, payload.to_vec()), (sequence, payload.to_vec())]
                }
            },
            2,
            "",
        );
        device.write(&[0x05, 0x01]).unwrap();
        let mut buf = [0u8; 64];
        assert_eq!(device.read(&mut buf, Duration::from_secs(1)).unwrap(), 2);
        assert_eq!(buf[..2], [0x05, 0x01]);
        assert_eq!(device.retransmissions(), 1);
        handle.join().unwrap();
        // The duplicate is still in the socket and must not be taken for the next reply.
        assert!(device.outstanding.is_empty());
    }

    #[test]
    fn a_silent_probe_times_out() {
        let (mut device, handle) = probe(|_, _, _| Vec::new(), 1, "");
        device.write(&[0x00, 0xFF]).unwrap();
        let mut buf = [0u8; 64];
        let error = device.read(&mut buf, Duration::from_millis(1)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        handle.join().unwrap();
    }
}
