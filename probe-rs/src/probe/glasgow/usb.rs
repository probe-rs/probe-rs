use std::{io, mem};

use async_io::block_on;
use futures_lite::future;
use nusb::{
    Interface, MaybeFuture,
    transfer::{Buffer, Bulk, Direction, In, Out},
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

    pub fn transfer(
        &mut self,
        output: Vec<u8>,
        mut input: impl FnMut(Vec<u8>) -> Result<bool, DebugProbeError>,
    ) -> Result<(), DebugProbeError> {
        let out_iface = self.out_iface.clone();
        let in_iface = self.in_iface.clone();
        let out_ep = self.out_ep_num;
        let in_ep = self.in_ep_num;

        block_on(async move {
            let out_fut = async move {
                if !output.is_empty() {
                    tracing::trace!("OUT URB: {}", hexdump(&output));
                    let out_len = output.len();
                    let mut endpoint = out_iface
                        .endpoint::<Bulk, Out>(out_ep)
                        .map_err(|e| DebugProbeError::Usb(e.into()))?;

                    let buffer = Buffer::from(output);
                    endpoint.submit(buffer);

                    let completion = endpoint.next_complete().await;
                    completion
                        .status
                        .map_err(io::Error::from)
                        .map_err(DebugProbeError::Usb)?;

                    if completion.actual_len != out_len {
                        return Err(DebugProbeError::Other(format!(
                            "expected to send {out_len} bytes, sent {}",
                            completion.actual_len
                        )));
                    }
                }
                Ok(())
            };

            let in_fut = async move {
                let mut endpoint = in_iface
                    .endpoint::<Bulk, In>(in_ep)
                    .map_err(|e| DebugProbeError::Usb(e.into()))?;

                let mut buffer = Vec::new();
                loop {
                    if input(mem::take(&mut buffer))? {
                        break;
                    }

                    let transfer = Buffer::new(65536);
                    endpoint.submit(transfer);

                    let completion = endpoint.next_complete().await;
                    completion
                        .status
                        .map_err(io::Error::from)
                        .map_err(DebugProbeError::Usb)?;

                    let data = completion.buffer.into_vec();
                    tracing::trace!("IN URB: {}", hexdump(&data));
                    buffer = data;
                }

                Ok::<(), DebugProbeError>(())
            };

            let (out_result, in_result) = future::zip(out_fut, in_fut).await;
            out_result.and(in_result)
        })
    }
}
