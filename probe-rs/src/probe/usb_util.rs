//! USB bulk transfer utilities.

use nusb::{
    Endpoint, Interface,
    transfer::{Buffer, Bulk, In, Out},
};
use std::fmt::Write;
use std::{io, time::Duration};

/// Encode a usb serial number as a hex
pub(crate) fn to_hex(s: &str) -> String {
    s.as_bytes().iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02X}"); // Writing a String never fails
        s
    })
}

/// Submit a single buffer to a bulk OUT endpoint and wait for it to complete.
///
/// On timeout, the pending transfer is cancelled and drained so the endpoint is
/// left with no outstanding transfers, and a `TimedOut` error is returned.
pub fn write_bulk_endpoint(
    endpoint: &mut Endpoint<Bulk, Out>,
    buf: &[u8],
    timeout: Duration,
) -> io::Result<usize> {
    let mut transfer_buffer = Buffer::new(buf.len());
    transfer_buffer.extend_from_slice(buf);

    endpoint.submit(transfer_buffer);

    let Some(completion) = endpoint.wait_next_complete(timeout) else {
        // Request cancellation...
        endpoint.cancel_all();

        // ...and then immediately drain the completion. Whether the the response is the
        // result of the original write, the cancellation, or a timeout of the cancellation, we
        // drop it and return a timeout.
        let _ = endpoint.wait_next_complete(Duration::from_millis(100));

        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "bulk write timed out",
        ));
    };

    completion.status.map_err(io::Error::from)?;

    Ok(completion.actual_len)
}

/// Submit a read to a bulk IN endpoint and wait for it to complete, copying the
/// received bytes into `buf`.
///
/// On timeout, the pending transfer is cancelled and drained so the endpoint is
/// left with no outstanding transfers, and a `TimedOut` error is returned.
pub fn read_bulk_endpoint(
    endpoint: &mut Endpoint<Bulk, In>,
    buf: &mut [u8],
    timeout: Duration,
) -> io::Result<usize> {
    let max_packet_size = endpoint.max_packet_size().max(1);
    let requested_len = buf.len().div_ceil(max_packet_size) * max_packet_size;

    let transfer_buffer = Buffer::new(requested_len);
    endpoint.submit(transfer_buffer);

    let Some(completion) = endpoint.wait_next_complete(timeout) else {
        // Request cancellation...
        endpoint.cancel_all();

        // ...and then immediately drain the completion. Whether the the response is the
        // result of the original read, the cancellation, or a timeout of the cancellation, we
        // drop it and return a timeout.
        let _ = endpoint.wait_next_complete(Duration::from_millis(100));

        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "bulk read timed out",
        ));
    };

    completion.status.map_err(io::Error::from)?;

    let actual_len = completion.actual_len;
    let data = completion.buffer;

    if actual_len > buf.len() || data.len() > buf.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "device returned {actual_len} bytes, buffer length is {}",
                buf.len()
            ),
        ));
    }

    buf[..actual_len].copy_from_slice(&data[..actual_len]);

    Ok(actual_len)
}

/// USB bulk transfer utility functions.
///
/// These claim a fresh endpoint for every transfer. For hot paths where the
/// same endpoint is used repeatedly, claim an [`Endpoint`] once and use
/// [`write_bulk_endpoint`] / [`read_bulk_endpoint`] instead to avoid the
/// per-transfer endpoint setup/teardown cost.
pub trait InterfaceExt {
    /// Reads data from the given bulk endpoint into the provided buffer.
    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;

    /// Writes data to the given bulk endpoint from the provided buffer.
    fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize>;
}

impl InterfaceExt for Interface {
    fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        let mut endpoint = self
            .endpoint::<Bulk, Out>(endpoint)
            .map_err(io::Error::from)?;

        write_bulk_endpoint(&mut endpoint, buf, timeout)
    }

    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let mut endpoint = self
            .endpoint::<Bulk, In>(endpoint)
            .map_err(io::Error::from)?;

        read_bulk_endpoint(&mut endpoint, buf, timeout)
    }
}
