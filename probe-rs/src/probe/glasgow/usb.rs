use std::io;

use async_io::block_on;
use futures_lite::future;
use nusb::{
    Interface,
    transfer::{Direction, RequestBuffer},
};

use crate::probe::{
    DebugProbeError, DebugProbeSelector, ProbeCreationError,
    glasgow::mux::{DiscoveryError, hexdump},
};

pub struct GlasgowUsbDevice {
    out_iface: Interface,
    in_iface: Interface,
    out_ep_num: u8,
    in_ep_num: u8,
}

impl GlasgowUsbDevice {
    pub fn new_from_selector(selector: &DebugProbeSelector) -> Result<Self, ProbeCreationError> {
        let Some(serial) = selector.serial_number.clone() else {
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
            .map_err(ProbeCreationError::Usb)?
            .find(|device| selector.matches(device))
            .ok_or(ProbeCreationError::NotFound)?;
        let device = device_info.open().map_err(ProbeCreationError::Usb)?;

        let mut in_ep_num = None;
        let mut out_ep_num = None;
        if let Ok(config) = device.active_configuration() {
            if let Some(interface) = config.interfaces().nth(in_iface_num as usize) {
                if let Some(altsetting) = interface.alt_settings().nth(1) {
                    if let Some(endpoint) = altsetting.endpoints().next() {
                        if endpoint.direction() == Direction::In {
                            in_ep_num = Some(endpoint.address());
                        }
                    }
                }
            }
            if let Some(interface) = config.interfaces().nth(out_iface_num as usize) {
                if let Some(altsetting) = interface.alt_settings().nth(1) {
                    if let Some(endpoint) = altsetting.endpoints().next() {
                        if endpoint.direction() == Direction::Out {
                            out_ep_num = Some(endpoint.address());
                        }
                    }
                }
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
            .map_err(ProbeCreationError::Usb)?;
        let in_iface = device
            .claim_interface(in_iface_num)
            .map_err(ProbeCreationError::Usb)?;

        // This takes the applet out of reset.
        out_iface
            .set_alt_setting(1)
            .map_err(ProbeCreationError::Usb)?;
        in_iface
            .set_alt_setting(1)
            .map_err(ProbeCreationError::Usb)?;

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
        block_on(async {
            let out_fut = async {
                if !output.is_empty() {
                    tracing::trace!("OUT URB: {}", hexdump(&output));
                    let out_buffer_len = output.len();
                    let out_completion = self.out_iface.bulk_out(self.out_ep_num, output).await;
                    out_completion
                        .status
                        .map_err(io::Error::other)
                        .map_err(DebugProbeError::Usb)?;
                    assert!(out_completion.data.actual_length() == out_buffer_len);
                }
                Ok(())
            };
            let in_fut = async {
                let mut buffer = Vec::new();
                while !input(buffer)? {
                    let in_completion = self
                        .in_iface
                        .bulk_in(self.in_ep_num, RequestBuffer::new(65536))
                        .await;
                    in_completion
                        .status
                        .map_err(io::Error::other)
                        .map_err(DebugProbeError::Usb)?;
                    tracing::trace!("IN URB: {}", hexdump(in_completion.data.as_slice()));
                    buffer = in_completion.data;
                }
                Ok::<(), DebugProbeError>(())
            };
            let (out_result, in_result) = future::zip(out_fut, in_fut).await;
            out_result.and(in_result)
        })
    }
}
