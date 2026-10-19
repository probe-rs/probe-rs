//! CMSIS-DAP over TCP.
//!
//! Uses the framing of OpenOCD's `cmsis-dap backend tcp`: every command and every response is
//! preceded by an 8 byte header, the signature `DAP\0`, the payload length as a little endian
//! `u16`, the packet type (1 for a request, 2 for a response) and a reserved byte. The payload is
//! the same as the payload of a CMSIS-DAP v2 bulk transfer.
//!
//! A TCP probe has no USB identity, so it is selected with a placeholder VID:PID and its address
//! as the serial number, for example `--probe 0000:0001:192.168.1.50:4441`.

use std::io::{self, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Placeholder vendor id that routes a selector to the TCP transport.
pub(crate) const TCP_VID: u16 = 0x0000;

/// Placeholder product id that routes a selector to the TCP transport.
pub(crate) const TCP_PID: u16 = 0x0001;

/// Port used when the address does not name one, the same as OpenOCD's default.
const DEFAULT_PORT: u16 = 4441;

const SIGNATURE: [u8; 4] = *b"DAP\0";
const TYPE_REQUEST: u8 = 1;
const TYPE_RESPONSE: u8 = 2;
const HEADER_LEN: usize = 8;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Shortest wait for a response. The timeouts used while draining and resynchronising are sized
/// for USB, and a reply over a network link, Wi-Fi in particular, takes longer than they allow.
/// OpenOCD uses the same floor for its TCP backend.
const MIN_READ_TIMEOUT: Duration = Duration::from_millis(150);

/// A connection to a CMSIS-DAP probe over TCP.
pub struct TcpDevice {
    stream: TcpStream,
    address: String,
    /// Bytes received but not yet returned, at most one partial frame after a timeout.
    pending: Vec<u8>,
}

impl TcpDevice {
    /// Connect to `address`, given as `host`, `host:port` or `[ipv6]:port`.
    pub fn connect(address: &str) -> io::Result<Self> {
        let mut last_error = io::Error::new(ErrorKind::NotFound, "address resolved to nothing");
        for socket_address in resolve(address)? {
            match TcpStream::connect_timeout(&socket_address, CONNECT_TIMEOUT) {
                Ok(stream) => {
                    stream.set_nodelay(true)?;
                    return Ok(Self {
                        stream,
                        address: address.to_string(),
                        pending: Vec::new(),
                    });
                }
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    /// The address the device was opened with.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Send one command.
    pub fn write(&mut self, payload: &[u8]) -> io::Result<usize> {
        let length = u16::try_from(payload.len())
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "packet too long"))?;
        let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
        frame.extend_from_slice(&SIGNATURE);
        frame.extend_from_slice(&length.to_le_bytes());
        frame.extend_from_slice(&[TYPE_REQUEST, 0]);
        frame.extend_from_slice(payload);
        self.stream.write_all(&frame)?;
        Ok(payload.len())
    }

    /// Receive one response into `buf`, returning its length.
    ///
    /// Fails with [`ErrorKind::TimedOut`] if no complete response arrives within `timeout`. Bytes
    /// of a partial response are kept for the next call.
    pub fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let deadline = Instant::now() + timeout.max(MIN_READ_TIMEOUT);
        loop {
            if let Some(length) = self.take_response(buf)? {
                return Ok(length);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ErrorKind::TimedOut.into());
            }
            self.stream.set_read_timeout(Some(remaining))?;

            let mut chunk = [0u8; 2048];
            match self.stream.read(&mut chunk).map_err(timed_out)? {
                0 => return Err(ErrorKind::ConnectionAborted.into()),
                n => self.pending.extend_from_slice(&chunk[..n]),
            }
        }
    }

    /// Move one complete response from `pending` to `buf`, if there is one.
    fn take_response(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        if self.pending.len() < HEADER_LEN {
            return Ok(None);
        }
        let header = &self.pending[..HEADER_LEN];
        if header[..4] != SIGNATURE || header[6] != TYPE_RESPONSE {
            // The stream cannot be resynchronised byte by byte; drop what was received.
            self.pending.clear();
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "invalid CMSIS-DAP TCP response header",
            ));
        }
        let length = u16::from_le_bytes([header[4], header[5]]) as usize;
        if self.pending.len() < HEADER_LEN + length {
            return Ok(None);
        }
        if length > buf.len() {
            self.pending.drain(..HEADER_LEN + length);
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "CMSIS-DAP TCP response longer than the packet size",
            ));
        }
        buf[..length].copy_from_slice(&self.pending[HEADER_LEN..HEADER_LEN + length]);
        self.pending.drain(..HEADER_LEN + length);
        Ok(Some(length))
    }
}

/// A socket read timeout is `WouldBlock` on Unix and `TimedOut` on Windows; report both as `TimedOut`.
fn timed_out(error: io::Error) -> io::Error {
    match error.kind() {
        ErrorKind::WouldBlock => ErrorKind::TimedOut.into(),
        _ => error,
    }
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
    use std::net::TcpListener;
    use std::thread;

    /// Accept one connection on a loopback port and hand it to `serve`.
    fn server(
        serve: impl FnOnce(TcpStream) + Send + 'static,
    ) -> (TcpDevice, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let handle = thread::spawn(move || serve(listener.accept().unwrap().0));
        (TcpDevice::connect(&address).unwrap(), handle)
    }

    fn response(payload: &[u8]) -> Vec<u8> {
        let mut frame = b"DAP\0".to_vec();
        frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        frame.extend_from_slice(&[TYPE_RESPONSE, 0]);
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn responses_are_reassembled_from_the_stream() {
        let (mut device, handle) = server(|mut stream| {
            // One response split across two segments, then two in one segment.
            let frame = response(&[0x00, 0x02, 0x00, 0x04]);
            stream.write_all(&frame[..5]).unwrap();
            thread::sleep(Duration::from_millis(20));
            stream.write_all(&frame[5..]).unwrap();
            let mut frames = response(&[0x05, 0x01, 0x01]);
            frames.extend(response(&[0x06, 0x00, 0x00, 0x01]));
            stream.write_all(&frames).unwrap();
        });
        let mut buf = [0u8; 64];
        let timeout = Duration::from_secs(1);
        assert_eq!(device.read(&mut buf, timeout).unwrap(), 4);
        assert_eq!(buf[..4], [0x00, 0x02, 0x00, 0x04]);
        assert_eq!(device.read(&mut buf, timeout).unwrap(), 3);
        assert_eq!(buf[0], 0x05);
        assert_eq!(device.read(&mut buf, timeout).unwrap(), 4);
        assert_eq!(buf[0], 0x06);
        handle.join().unwrap();
    }

    #[test]
    fn late_response_is_read_after_a_timeout() {
        // A reply that arrives after its command timed out has to come out of the next read in one
        // piece, so that the command layer sees a late reply instead of a corrupted one.
        let (mut device, handle) = server(|mut stream| {
            let frame = response(&[0x00, 0x02, 0x00, 0x04]);
            stream.write_all(&frame[..5]).unwrap();
            thread::sleep(MIN_READ_TIMEOUT * 2);
            stream.write_all(&frame[5..]).unwrap();
        });
        let mut buf = [0u8; 64];
        let error = device.read(&mut buf, Duration::ZERO).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert_eq!(device.read(&mut buf, Duration::from_secs(1)).unwrap(), 4);
        assert_eq!(buf[..4], [0x00, 0x02, 0x00, 0x04]);
        handle.join().unwrap();
    }

    #[test]
    fn bad_header_is_rejected() {
        let (mut device, handle) = server(|mut stream| {
            let mut frame = response(&[0x00]);
            frame[0] = b'X';
            stream.write_all(&frame).unwrap();
        });
        let mut buf = [0u8; 64];
        let error = device.read(&mut buf, Duration::from_secs(1)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        handle.join().unwrap();
    }

    #[test]
    fn closed_connection_is_an_error() {
        let (mut device, handle) = server(drop);
        handle.join().unwrap();
        let mut buf = [0u8; 64];
        let error = device.read(&mut buf, Duration::from_secs(1)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ConnectionAborted);
    }
}
