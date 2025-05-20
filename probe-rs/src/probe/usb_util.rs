use futures_lite::FutureExt;
use nusb::{
    Interface,
    transfer::{Bulk, In, Out},
};
use std::{io, time::Duration};

pub trait InterfaceExt {
    async fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration)
    -> io::Result<usize>;
    async fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize>;
}

impl InterfaceExt for Interface {
    async fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        let fut = async {
            let mut ep_out = self.endpoint::<Bulk, Out>(endpoint).unwrap();
            let mut transfer = ep_out.allocate(buf.len());
            transfer.extend_from_slice(buf);
            ep_out.submit(transfer);
            let comp = ep_out.next_complete().await;
            comp.status.map_err(io::Error::other)?;
            let n = comp.actual_len;

            Ok::<usize, io::Error>(n)
        };

        fut.or(async {
            wait(timeout).await;
            Err(std::io::ErrorKind::TimedOut.into())
        })
        .await
    }

    async fn read_bulk(
        &self,
        endpoint: u8,
        buf: &mut [u8],
        timeout: Duration,
    ) -> io::Result<usize> {
        let fut = async {
            let mut ep_in = self.endpoint::<Bulk, In>(endpoint).unwrap();
            let transfer = ep_in.allocate(buf.len());
            ep_in.submit(transfer);
            let comp = ep_in.next_complete().await;
            comp.status.map_err(io::Error::other)?;

            let n = comp.actual_len;
            // If we got some number of bytes that is divisible by
            // wMaxPacketSize (and is nonzero), then we'll need to read a
            // zero length packet.
            if n != 0 && n % 64 == 0 {
                ep_in.submit(ep_in.allocate(0));
                ep_in.next_complete().await;
            }
            buf[..n].copy_from_slice(&comp.buffer);
            Ok::<usize, io::Error>(n)
        };

        fut.or(async {
            wait(timeout).await;
            Err(std::io::ErrorKind::TimedOut.into())
        })
        .await
    }
}

#[cfg(target_family = "wasm")]
pub async fn wait(timeout: Duration) {
    pub(crate) fn set_timeout(resolve: wasm_bindgen_futures::js_sys::Function, ms: i32) {
        let window = wasm_bindgen::JsCast::dyn_into::<web_sys::Window>(
            wasm_bindgen_futures::js_sys::global(),
        )
        .ok();

        if let Some(window) = window {
            window
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
                .expect("timeouts work");
            return;
        }

        let wgs = wasm_bindgen::JsCast::dyn_into::<web_sys::WorkerGlobalScope>(
            wasm_bindgen_futures::js_sys::global(),
        )
        .ok();

        if let Some(wgs) = wgs {
            wgs.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
                .expect("timeouts work");
            return;
        }

        panic!("Timeout could not be set")
    }

    let promise = wasm_bindgen_futures::js_sys::Promise::new(&mut |resolve, _| {
        set_timeout(resolve, timeout.as_millis() as i32)
    });

    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .expect("promise completes without issues");
}

#[cfg(not(target_family = "wasm"))]
pub async fn wait(timeout: Duration) {
    async_io::Timer::after(timeout).await;
}
