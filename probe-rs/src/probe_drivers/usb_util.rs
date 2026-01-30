use nusb::{
    Interface,
    transfer::{Buffer, Bulk, In, Out},
};
use std::{io, time::Duration};

pub trait InterfaceExt {
    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
    fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize>;
}

impl InterfaceExt for Interface {
    fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        let mut endpoint = self
            .endpoint::<Bulk, Out>(endpoint)
            .map_err(io::Error::from)?;

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

    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let mut endpoint = self
            .endpoint::<Bulk, In>(endpoint)
            .map_err(io::Error::from)?;

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
}

impl crate::probe::DebugProbeSelector {
    pub(crate) fn matches(&self, info: &nusb::DeviceInfo) -> bool {
        if self.interface.is_some() {
            info.interfaces().any(|iface| {
                self.match_probe_selector(
                    info.vendor_id(),
                    info.product_id(),
                    Some(iface.interface_number()),
                    info.serial_number(),
                )
            })
        } else {
            self.match_probe_selector(
                info.vendor_id(),
                info.product_id(),
                None,
                info.serial_number(),
            )
        }
    }
}
