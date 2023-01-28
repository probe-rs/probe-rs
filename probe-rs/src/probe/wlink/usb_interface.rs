use std::time::Duration;

use rusb::{DeviceHandle, UsbContext};

use crate::{DebugProbeError, DebugProbeSelector, ProbeCreationError};

use super::{get_wlink_info, WchLinkError};

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

        tracing::debug!("Acquired libusb context.");
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

        tracing::debug!("Aquired handle for probe");

        let config = device.active_config_descriptor()?;

        tracing::debug!("Active config descriptor: {:?}", &config);

        let descriptor = device.device_descriptor()?;

        tracing::debug!("Device descriptor: {:?}", &descriptor);

        device_handle.claim_interface(0)?;

        tracing::debug!("Claimed interface 0 of USB device.");

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

    pub(crate) fn write_command(
        &mut self,
        cmd: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, DebugProbeError> {
        tracing::trace!(
            "Sending command {:02x?} to WCH-Link, timeout: {:?}",
            cmd,
            timeout
        );

        // Command phase.
        let written_bytes = self
            .device_handle
            .write_bulk(ENDPOINT_OUT, cmd, timeout)
            .map_err(|e| DebugProbeError::Usb(Some(Box::new(e))))?;

        if written_bytes != cmd.len() {
            return Err(WchLinkError::NotEnoughBytesWritten {
                is: written_bytes,
                should: cmd.len(),
            }
            .into());
        }

        // data in phase.
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

        tracing::trace!(
            "Receive response {:02x?} from WCH-Link",
            &rxbuf[..read_bytes]
        );
        Ok(rxbuf[..read_bytes].to_vec())
    }

    pub(crate) fn write(
        &mut self,
        cmd: &[u8],
        read_data: &mut [u8],
        timeout: Duration,
    ) -> Result<(), DebugProbeError> {
        tracing::trace!(
            "Sending command {:02x?} to WCH-Link, timeout: {:?}",
            cmd,
            timeout
        );

        // Command phase.
        let written_bytes = self
            .device_handle
            .write_bulk(ENDPOINT_OUT, cmd, timeout)
            .map_err(|e| DebugProbeError::Usb(Some(Box::new(e))))?;

        if written_bytes != cmd.len() {
            return Err(WchLinkError::NotEnoughBytesWritten {
                is: written_bytes,
                should: cmd.len(),
            }
            .into());
        }

        // data in phase.
        let mut remaining_bytes = read_data.len();
        let mut read_index = 0;

        while remaining_bytes > 0 {
            let read_bytes = self
                .device_handle
                .read_bulk(ENDPOINT_IN, &mut read_data[read_index..], timeout)
                .map_err(|e| DebugProbeError::Usb(Some(Box::new(e))))?;

            read_index += read_bytes;
            remaining_bytes -= read_bytes;

            if remaining_bytes > 0 {
                tracing::trace!(
                    "Read {} bytes, {} bytes remaining, buf {:02x?}",
                    read_bytes,
                    remaining_bytes,
                    &read_data[..read_index]
                );
            }
        }
        tracing::trace!("Receive response {:02x?} from WCH-Link", read_data);
        Ok(())
    }
}

impl Drop for WchLinkUsbDevice {
    fn drop(&mut self) {
        // We ignore the error case as we can't do much about it anyways.
        let _ = self.close();
    }
}
