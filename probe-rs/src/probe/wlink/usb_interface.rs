use std::time::Duration;

use rusb::{DeviceHandle, UsbContext};

use crate::{DebugProbeError, DebugProbeSelector, ProbeCreationError};

use super::{commands::WchLinkCommand, get_wlink_info, WchLinkError};

const ENDPOINT_OUT: u8 = 0x01;
const ENDPOINT_IN: u8 = 0x81;

// const RAW_ENDPOINT_OUT: u8 = 0x02;
// const RAW_ENDPOINT_IN: u8 = 0x82;

#[derive(Debug)]
pub struct WchLinkUsbDevice {
    device_handle: DeviceHandle<rusb::Context>,
}

impl WchLinkUsbDevice {
    pub fn new_from_selector(
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Self, ProbeCreationError> {
        let selector = selector.into();

        let context = rusb::Context::new()?;

        tracing::trace!("Acquired libusb context.");
        let device = context
            .devices()?
            .iter()
            .filter(|device| {
                device
                    .device_descriptor()
                    .map(|desc| {
                        desc.vendor_id() == selector.vendor_id
                            && desc.product_id() == selector.product_id
                    })
                    .unwrap_or(false)
            })
            .find(|device| get_wlink_info(device).is_some())
            .map_or(Err(ProbeCreationError::NotFound), Ok)?;

        let mut device_handle = device.open()?;

        tracing::trace!("Aquired handle for probe");

        let config = device.active_config_descriptor()?;

        tracing::trace!("Active config descriptor: {:?}", &config);

        let descriptor = device.device_descriptor()?;

        tracing::trace!("Device descriptor: {:?}", &descriptor);

        device_handle.claim_interface(0)?;

        tracing::trace!("Claimed interface 0 of USB device.");

        let mut endpoint_out = false;
        let mut endpoint_in = false;

        if let Some(interface) = config.interfaces().next() {
            if let Some(descriptor) = interface.descriptors().next() {
                for endpoint in descriptor.endpoint_descriptors() {
                    if endpoint.address() == ENDPOINT_OUT {
                        endpoint_out = true;
                    } else if endpoint.address() == ENDPOINT_IN {
                        endpoint_in = true;
                    }
                }
            }
        }

        if !endpoint_out {
            return Err(WchLinkError::EndpointNotFound.into());
        }

        if !endpoint_in {
            return Err(WchLinkError::EndpointNotFound.into());
        }

        let usb_wlink = Self { device_handle };

        tracing::debug!("Succesfully attached to WCH-Link.");

        Ok(usb_wlink)
    }

    fn close(&mut self) -> Result<(), rusb::Error> {
        self.device_handle.release_interface(0)
    }

    pub(crate) fn send_command<C: WchLinkCommand + std::fmt::Debug>(
        &mut self,
        cmd: C,
    ) -> Result<C::Response, DebugProbeError> {
        tracing::debug!("Sending command: {:?}", cmd);

        let mut rxbuf = [0u8; 64];
        let len = cmd.to_bytes(&mut rxbuf)?;

        let timeout = Duration::from_millis(100);

        let written_bytes = self
            .device_handle
            .write_bulk(ENDPOINT_OUT, &rxbuf[..len], timeout)
            .map_err(|e| DebugProbeError::Usb(Some(Box::new(e))))?;

        if written_bytes != len {
            return Err(WchLinkError::NotEnoughBytesWritten {
                is: written_bytes,
                should: len,
            }
            .into());
        }

        let mut rxbuf = [0u8; 64];
        let read_bytes = self
            .device_handle
            .read_bulk(ENDPOINT_IN, &mut rxbuf[..], timeout)
            .map_err(|e| DebugProbeError::Usb(Some(Box::new(e))))?;

        if read_bytes < 3 {
            return Err(WchLinkError::NotEnoughBytesRead {
                is: read_bytes,
                should: 3,
            }
            .into());
        }
        if read_bytes != rxbuf[2] as usize + 3 {
            return Err(WchLinkError::NotEnoughBytesRead {
                is: read_bytes,
                should: 3 + (rxbuf[2] as usize),
            }
            .into());
        }

        let response = cmd.parse_response(&rxbuf[..read_bytes])?;

        Ok(response)
    }
}

impl Drop for WchLinkUsbDevice {
    fn drop(&mut self) {
        // We ignore the error case as we can't do much about it anyways.
        let _ = self.close();
    }
}
