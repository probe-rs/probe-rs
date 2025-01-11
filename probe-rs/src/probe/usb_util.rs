use nusb::{transfer::RequestBuffer, Interface};
use std::{io, time::Duration};

pub trait InterfaceExt {
    async fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration)
        -> io::Result<usize>;
    async fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize>;
}

impl InterfaceExt for Interface {
    async fn write_bulk(&self, endpoint: u8, buf: &[u8], _timeout: Duration) -> io::Result<usize> {
        let fut = async {
            let comp = self.bulk_out(endpoint, buf.to_vec()).await;
            comp.status.map_err(io::Error::other)?;

            let n = comp.data.actual_length();
            Ok::<usize, io::Error>(n)
        };

        fut.await
    }

    async fn read_bulk(
        &self,
        endpoint: u8,
        buf: &mut [u8],
        _timeout: Duration,
    ) -> io::Result<usize> {
        let fut = async {
            let comp = self.bulk_in(endpoint, RequestBuffer::new(buf.len())).await;
            comp.status.map_err(io::Error::other)?;

            let n = comp.data.len();
            buf[..n].copy_from_slice(&comp.data);
            Ok::<usize, io::Error>(n)
        };

        fut.await
    }
}
