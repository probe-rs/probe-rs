//! XVC (Xilinx Virtual Cable) wire protocol.
//!
//! XVC is a small TCP-based protocol that tunnels JTAG over a network
//! connection. It only defines three commands:
//!
//! - `getinfo:` returns the server version and the maximum vector length
//!   (in bytes) that can be shifted in a single operation.
//! - `settck:<period>` sets the TCK period in nanoseconds.
//! - `shift:<num_bits><tms><tdi>` clocks `num_bits` bits through the TAP and
//!   returns the sampled TDO bits.
//!
//! For each command the bit vectors are LSB-first, packed into
//! `ceil(num_bits / 8)` bytes. This module buffers individual bits and flushes
//! them to the server in batches using a single `shift:` command.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use bitvec::prelude::*;

use crate::probe::{DebugProbeError, DebugProbeSelector, ProbeCreationError, ProbeError};

use super::{XVC_PID, XVC_VID};

/// Default TCP port used by XVC servers.
const DEFAULT_PORT: u16 = 2542;

/// Lower bound applied to the buffer size reported by `getinfo:`.
const MIN_SHIFT_BITS: usize = 64;

/// Upper bound applied to the buffer size reported by `getinfo:`, to guard
/// against pathological allocations.
const MAX_SHIFT_BITS: usize = 64 * 1024 * 8;

/// How long to wait for the server's reply to the `getinfo:` handshake before
/// concluding that the peer is not an XVC server.
const GETINFO_TIMEOUT: Duration = Duration::from_secs(2);

/// Errors specific to the XVC probe.
#[derive(Debug, thiserror::Error)]
pub enum XvcError {
    /// The server address could not be parsed.
    #[error("Invalid XVC server address {0:?}. Expected \"<host>\" or \"<host>:<port>\".")]
    InvalidAddress(String),

    /// Communication with the XVC server failed.
    #[error("Failed to communicate with the XVC server: {0}")]
    Io(#[from] std::io::Error),

    /// The server sent a response that could not be understood.
    #[error("Unexpected response from the XVC server: {0}")]
    Protocol(String),
}

impl ProbeError for XvcError {}

/// A connection to an XVC server.
pub struct XvcDevice {
    stream: TcpStream,
    address: String,

    /// Maximum number of bits that can be shifted in a single `shift:` command.
    max_shift_bits: usize,
    speed_khz: u32,

    /// Pending TMS bits, packed for direct transmission.
    tms: BitVec<u8, Lsb0>,
    /// Pending TDI bits, packed for direct transmission.
    tdi: BitVec<u8, Lsb0>,
    /// For each pending bit, whether its TDO output should be captured.
    capture: BitVec<u8, Lsb0>,
    /// Captured TDO bits, waiting to be read out.
    response: BitVec,
}

impl std::fmt::Debug for XvcDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XvcDevice")
            .field("address", &self.address)
            .field("max_shift_bits", &self.max_shift_bits)
            .field("speed_khz", &self.speed_khz)
            .finish()
    }
}

impl XvcDevice {
    pub(crate) fn new_from_selector(
        selector: &DebugProbeSelector,
    ) -> Result<Self, ProbeCreationError> {
        // Only handle selectors that explicitly target an XVC endpoint, so that
        // we never try to open a TCP connection for unrelated probes.
        if selector.vendor_id != XVC_VID || selector.product_id != XVC_PID {
            return Err(ProbeCreationError::NotFound);
        }

        let Some(serial) = selector.serial_number.as_deref() else {
            return Err(ProbeCreationError::NotFound);
        };

        let address = normalize_address(serial)?;

        let mut stream = TcpStream::connect(&address).map_err(XvcError::from)?;
        // Disable Nagle's algorithm: XVC is a strict request/response protocol,
        // so batching writes only adds latency.
        stream.set_nodelay(true).map_err(XvcError::from)?;

        tracing::info!("Connected to {address}");

        // Validate that the peer is actually an XVC server (and learn its shift
        // buffer size) before sending any `shift:` commands.
        let max_shift_bits = handshake(&mut stream)?;
        tracing::debug!("XVC server at {address} reports a {max_shift_bits}-bit shift buffer");

        Ok(Self {
            stream,
            address,
            max_shift_bits,
            speed_khz: 1000,
            tms: BitVec::new(),
            tdi: BitVec::new(),
            capture: BitVec::new(),
            response: BitVec::new(),
        })
    }

    pub(crate) fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    pub(crate) fn set_speed_khz(&mut self, speed_khz: u32) -> u32 {
        // XVC defines a `settck:` command, but many minimal servers ignore it or
        // do not implement it at all, so we only record the requested speed.
        self.speed_khz = speed_khz;
        self.speed_khz
    }

    /// Buffers a single bit, flushing first if the buffer is full.
    pub(crate) fn shift_bit(
        &mut self,
        tms: bool,
        tdi: bool,
        capture: bool,
    ) -> Result<(), DebugProbeError> {
        if self.tms.len() >= self.max_shift_bits {
            self.flush()?;
        }

        self.tms.push(tms);
        self.tdi.push(tdi);
        self.capture.push(capture);

        Ok(())
    }

    /// Flushes any pending bits and returns the captured TDO bits.
    pub(crate) fn read_captured_bits(&mut self) -> Result<BitVec, DebugProbeError> {
        self.flush()?;
        Ok(std::mem::take(&mut self.response))
    }

    /// Sends all pending bits to the server with a single `shift:` command and
    /// collects the requested TDO bits.
    fn flush(&mut self) -> Result<(), DebugProbeError> {
        let num_bits = self.tms.len();
        if num_bits == 0 {
            return Ok(());
        }

        // Zero the unused bits of the final byte so we transmit clean padding.
        self.tms.set_uninitialized(false);
        self.tdi.set_uninitialized(false);

        let byte_len = num_bits.div_ceil(8);

        let mut request = Vec::with_capacity(b"shift:".len() + 4 + byte_len * 2);
        request.extend_from_slice(b"shift:");
        request.extend_from_slice(&(num_bits as u32).to_le_bytes());
        request.extend_from_slice(self.tms.as_raw_slice());
        request.extend_from_slice(self.tdi.as_raw_slice());

        self.stream
            .write_all(&request)
            .map_err(|e| DebugProbeError::from(XvcError::from(e)))?;

        let mut tdo = vec![0u8; byte_len];
        self.stream
            .read_exact(&mut tdo)
            .map_err(|e| DebugProbeError::from(XvcError::from(e)))?;

        let tdo_bits = tdo.view_bits::<Lsb0>();
        for index in 0..num_bits {
            if self.capture[index] {
                self.response.push(tdo_bits[index]);
            }
        }

        self.tms.clear();
        self.tdi.clear();
        self.capture.clear();

        Ok(())
    }
}

/// Normalizes a user-supplied address into a `host:port` pair, applying the
/// default XVC port when none is given.
fn normalize_address(serial: &str) -> Result<String, XvcError> {
    let serial = serial.trim();
    if serial.is_empty() {
        return Err(XvcError::InvalidAddress(serial.to_string()));
    }

    // A colon means the port (or an IPv6 literal such as `[::1]:2542`) is
    // already present, so use the address verbatim. Otherwise append the
    // default port.
    if serial.contains(':') {
        Ok(serial.to_string())
    } else {
        Ok(format!("{serial}:{DEFAULT_PORT}"))
    }
}

/// Performs the XVC `getinfo:` handshake.
///
/// This both validates that the peer is an XVC server and returns the maximum
/// number of bits that can be shifted in a single command. A missing or
/// non-XVC reply is treated as a hard error, since it most likely means we
/// connected to something that is not an XVC server at all.
fn handshake(stream: &mut TcpStream) -> Result<usize, XvcError> {
    // Bound the reply by a read timeout so a peer that never answers cannot make
    // us hang. On any error below the connection is dropped, so we only need to
    // restore blocking reads (for the shift operations) on the success path.
    stream.set_read_timeout(Some(GETINFO_TIMEOUT))?;

    stream.write_all(b"getinfo:")?;

    // The reply has the form `xvcServer_v<version>:<max-bytes>\n`.
    let mut buffer = [0u8; 64];
    let read = stream.read(&mut buffer)?;
    let response = String::from_utf8_lossy(&buffer[..read]);
    let response = response.trim();

    if !response.starts_with("xvcServer") {
        return Err(XvcError::Protocol(format!(
            "unexpected reply to `getinfo:` ({response:?}); is this an XVC server?"
        )));
    }

    // The peer is confirmed to be an XVC server; parse its maximum shift length.
    let max_bytes = response
        .rsplit(':')
        .next()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .ok_or_else(|| {
            XvcError::Protocol(format!(
                "could not parse the buffer size from the `getinfo:` reply ({response:?})"
            ))
        })?;

    stream.set_read_timeout(None)?;

    Ok(max_bytes
        .saturating_mul(8)
        .clamp(MIN_SHIFT_BITS, MAX_SHIFT_BITS))
}
