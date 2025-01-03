use async_io::{block_on, Timer};
use futures_lite::FutureExt;
use nusb::{transfer::RequestBuffer, Interface};
use std::{io, time::Duration};

pub trait InterfaceExt {
    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
    fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize>;
}

impl InterfaceExt for Interface {
    fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        let fut = async {
            let comp = self.bulk_out(endpoint, buf.to_vec()).await;
            comp.status.map_err(io::Error::other)?;

            let n = comp.data.actual_length();
            Ok(n)
        };

        block_on(fut.or(async {
            Timer::after(timeout).await;
            Err(std::io::ErrorKind::TimedOut.into())
        }))
    }

    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let mut queue = self.bulk_in_queue(endpoint);
        queue.submit(RequestBuffer::new(buf.len()));
        let Some(comp) = block_on(
            async {
                let comp = queue.next_complete().await;
                Some(comp)
            }
            .or(async {
                Timer::after(timeout).await;
                None
            }),
        ) else {
            queue.cancel_all();
            let _ = block_on(queue.next_complete());
            return Err(std::io::ErrorKind::TimedOut.into());
        };
        comp.status.map_err(io::Error::other)?;
        let n = comp.data.len();
        buf[..n].copy_from_slice(&comp.data);
        Ok(n)
    }
}
