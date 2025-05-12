use futures_lite::FutureExt;
use nusb::{Interface, transfer::RequestBuffer};
use std::{io, time::Duration};

pub trait InterfaceExt {
    async fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration)
    -> io::Result<usize>;
    async fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize>;
}

impl InterfaceExt for Interface {
    async fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        let fut = async {
            let comp = self.bulk_out(endpoint, buf.to_vec()).await;
            comp.status.map_err(io::Error::other)?;

            let n = comp.data.actual_length();
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
            let comp = self.bulk_in(endpoint, RequestBuffer::new(buf.len())).await;
            comp.status.map_err(io::Error::other)?;

            let n = comp.data.len();
            buf[..n].copy_from_slice(&comp.data);
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
