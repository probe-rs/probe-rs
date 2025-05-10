use async_io::{Timer, block_on};
use futures_lite::FutureExt;
use nusb::{
    Interface,
    transfer::{Bulk, In, Out},
};
use std::{io, time::Duration};

pub trait InterfaceExt {
    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
    fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize>;
}

impl InterfaceExt for Interface {
    fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        let fut = async {
            let mut ep_out = self.endpoint::<Bulk, Out>(endpoint).unwrap();
            let mut transfer = ep_out.allocate(64);
            transfer.extend_from_slice(buf);
            ep_out.submit(transfer);
            let Some(comp) = ep_out.wait_next_complete(timeout) else {
                return Err(std::io::ErrorKind::TimedOut.into());
            };
            comp.status.map_err(io::Error::other)?;
            let n = comp.actual_len;
            Ok(n)
        };

        block_on(fut.or(async {
            Timer::after(timeout).await;
            Err(std::io::ErrorKind::TimedOut.into())
        }))
    }

    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let fut = async {
            let mut ep_in = self.endpoint::<Bulk, In>(endpoint).unwrap();
            let transfer = ep_in.allocate(buf.len());
            ep_in.submit(transfer);
            let Some(comp) = ep_in.wait_next_complete(timeout) else {
                return Err(std::io::ErrorKind::TimedOut.into());
            };
            comp.status.map_err(io::Error::other)?;

            let n = comp.actual_len;
            // If we got some number of bytes that is divisible by
            // wMaxPacketSize (and is nonzero), then we'll need to read a
            // zero length packet.
            if n != 0 && n % 64 == 0 {
                ep_in.submit(ep_in.allocate(0));
                ep_in.wait_next_complete(Duration::from_secs(1)).unwrap();
            }
            buf[..n].copy_from_slice(&comp.buffer);
            Ok(n)
        };

        block_on(fut.or(async {
            Timer::after(timeout).await;
            Err(std::io::ErrorKind::TimedOut.into())
        }))
    }
}
