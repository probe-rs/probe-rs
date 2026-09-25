use std::{
    io, mem,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    thread::{self, Thread},
};

use nusb::{
    Interface, MaybeFuture,
    transfer::{Buffer, Bulk, Completion, Direction, In, Out},
};

use crate::probe::{
    DebugProbeError, DebugProbeSelector, ProbeCreationError,
    glasgow::mux::{DiscoveryError, hexdump},
};

pub(super) const VID_QIHW: u16 = 0x20b7;
pub(super) const PID_GLASGOW: u16 = 0x9db1;

pub struct GlasgowUsbDevice {
    out_iface: Interface,
    in_iface: Interface,
    out_ep_num: u8,
    in_ep_num: u8,
}

impl GlasgowUsbDevice {
    pub fn new_from_selector(selector: &DebugProbeSelector) -> Result<Self, ProbeCreationError> {
        if selector.vendor_id != VID_QIHW && selector.product_id != PID_GLASGOW {
            Err(ProbeCreationError::NotFound)?
        }
        let Some(serial) = selector.serial_number.as_ref() else {
            Err(ProbeCreationError::NotFound)?
        };
        let parts = serial.split(":").collect::<Vec<_>>();
        let [serial, in_iface_num, out_iface_num] = parts[..] else {
            Err(DiscoveryError::InvalidFormat)?
        };
        let in_iface_num: u8 = in_iface_num
            .parse()
            .map_err(|_| DiscoveryError::InvalidFormat)?;
        let out_iface_num: u8 = out_iface_num
            .parse()
            .map_err(|_| DiscoveryError::InvalidFormat)?;

        let selector = DebugProbeSelector {
            serial_number: Some(serial.to_owned()),
            ..selector.clone()
        };
        let device_info = nusb::list_devices()
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?
            .find(|device| selector.matches(device))
            .ok_or(ProbeCreationError::NotFound)?;
        let device = device_info
            .open()
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;

        let mut in_ep_num = None;
        let mut out_ep_num = None;
        if let Ok(config) = device.active_configuration() {
            if let Some(interface) = config.interfaces().nth(in_iface_num as usize)
                && let Some(altsetting) = interface.alt_settings().nth(1)
                && let Some(endpoint) = altsetting.endpoints().next()
                && endpoint.direction() == Direction::In
            {
                in_ep_num = Some(endpoint.address());
            }
            if let Some(interface) = config.interfaces().nth(out_iface_num as usize)
                && let Some(altsetting) = interface.alt_settings().nth(1)
                && let Some(endpoint) = altsetting.endpoints().next()
                && endpoint.direction() == Direction::Out
            {
                out_ep_num = Some(endpoint.address());
            }
        }

        let (Some(in_ep_num), Some(out_ep_num)) = (in_ep_num, out_ep_num) else {
            Err(DiscoveryError::InvalidInterfaces)?
        };
        tracing::info!(
            "opened Glasgow Interface Explorer (IN {in_iface_num}/{in_ep_num:#04x}, OUT {out_iface_num}/{out_ep_num:#04x})"
        );

        // This makes our endpoints available for use.
        let out_iface = device
            .claim_interface(out_iface_num)
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;
        let in_iface = device
            .claim_interface(in_iface_num)
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;

        // This takes the applet out of reset.
        out_iface
            .set_alt_setting(1)
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;
        in_iface
            .set_alt_setting(1)
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;

        Ok(Self {
            out_iface,
            in_iface,
            out_ep_num,
            in_ep_num,
        })
    }

    /// Perform a full-duplex bulk transfer.
    ///
    /// OUT and IN URBs are kept in flight concurrently, using
    /// [`nusb::Endpoint::poll_next_complete`] and a thread-park waker.
    pub fn transfer(
        &mut self,
        output: Vec<u8>,
        mut input: impl FnMut(Vec<u8>) -> Result<bool, DebugProbeError>,
    ) -> Result<(), DebugProbeError> {
        let mut out_endpoint = None;
        let mut out_done = true;
        let mut expected_out_len = 0;

        if !output.is_empty() {
            tracing::trace!("OUT URB: {}", hexdump(&output));
            expected_out_len = output.len();
            let mut endpoint = self
                .out_iface
                .endpoint::<Bulk, Out>(self.out_ep_num)
                .map_err(|e| DebugProbeError::Usb(e.into()))?;
            endpoint.submit(Buffer::from(output));
            out_endpoint = Some(endpoint);
            out_done = false;
        }

        let mut in_endpoint = self
            .in_iface
            .endpoint::<Bulk, In>(self.in_ep_num)
            .map_err(|e| DebugProbeError::Usb(e.into()))?;

        let mut buffer = Vec::new();
        let mut need_more_in = !input(mem::take(&mut buffer))?;
        let mut in_pending = false;

        block_on_poll(|cx| {
            loop {
                if need_more_in && !in_pending {
                    in_endpoint.submit(Buffer::new(65536));
                    in_pending = true;
                }

                if out_done && !need_more_in {
                    debug_assert!(!in_pending);
                    return Poll::Ready(Ok(()));
                }

                let mut pending = false;

                if !out_done {
                    let endpoint = out_endpoint
                        .as_mut()
                        .expect("OUT endpoint must exist while waiting for OUT");
                    match endpoint.poll_next_complete(cx) {
                        Poll::Ready(completion) => {
                            check_completion(&completion)?;
                            if completion.actual_len != expected_out_len {
                                return Poll::Ready(Err(DebugProbeError::Other(format!(
                                    "expected to send {expected_out_len} bytes, sent {}",
                                    completion.actual_len
                                ))));
                            }
                            out_done = true;
                            continue;
                        }
                        Poll::Pending => pending = true,
                    }
                }

                if in_pending {
                    match in_endpoint.poll_next_complete(cx) {
                        Poll::Ready(completion) => {
                            check_completion(&completion)?;
                            let data = completion.buffer.into_vec();
                            tracing::trace!("IN URB: {}", hexdump(&data));
                            buffer = data;
                            in_pending = false;
                            need_more_in = !input(mem::take(&mut buffer))?;
                            continue;
                        }
                        Poll::Pending => pending = true,
                    }
                }

                if pending {
                    return Poll::Pending;
                }
            }
        })
    }
}

fn check_completion(completion: &Completion) -> Result<(), DebugProbeError> {
    completion
        .status
        .map_err(io::Error::from)
        .map_err(DebugProbeError::Usb)
}

/// Drive a `Poll`-based operation to completion by parking the current thread
/// until nusb wakes it via [`Endpoint::poll_next_complete`].
fn block_on_poll<T>(mut poll: impl FnMut(&mut Context<'_>) -> Poll<T>) -> T {
    let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        match poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => thread::park(),
        }
    }
}

struct ThreadWake(Thread);

impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}
