use std::time::Duration;

use nusb::{Interface, MaybeFuture};

use crate::probe::{
    DebugProbeError, DebugProbeSelector, ProbeCreationError, usb_util::InterfaceExt,
};

use super::{WchLinkError, commands::WchLinkCommand, get_wlink_info};

const ENDPOINT_OUT: u8 = 0x01;
const ENDPOINT_IN: u8 = 0x81;

// const RAW_ENDPOINT_OUT: u8 = 0x02;
// const RAW_ENDPOINT_IN: u8 = 0x82;

pub struct WchLinkUsbDevice {
    device_handle: Interface,
}

impl WchLinkUsbDevice {
    pub fn new_from_selector(selector: &DebugProbeSelector) -> Result<Self, ProbeCreationError> {
        let device = nusb::list_devices()
            .wait()
            .map_err(ProbeCreationError::Usb)?
            .filter(|device| selector.matches(device))
            .find(|device| get_wlink_info(device).is_some())
            .ok_or(ProbeCreationError::NotFound)?;

        let mut endpoint_out = false;
        let mut endpoint_in = false;

        let device_handle = device.open().wait().map_err(ProbeCreationError::Usb)?;

        let mut configs = device_handle.configurations();
        if let Some(config) = configs.next() {
            if let Some(interface) = config.interfaces().next() {
                if let Some(altsetting) = interface.alt_settings().next() {
                    for endpoint in altsetting.endpoints() {
                        if endpoint.address() == ENDPOINT_OUT {
                            endpoint_out = true;
                        } else if endpoint.address() == ENDPOINT_IN {
                            endpoint_in = true;
                        }
                    }
                }
            }
        }

        if !endpoint_out || !endpoint_in {
            return Err(WchLinkError::EndpointNotFound.into());
        }

        tracing::trace!("Aquired handle for probe");
        let device_handle = device_handle
            .claim_interface(0)
            .wait()
            .map_err(ProbeCreationError::Usb)?;
        tracing::trace!("Claimed interface 0 of USB device.");

        let usb_wlink = Self { device_handle };

        tracing::debug!("Succesfully attached to WCH-Link.");

        Ok(usb_wlink)
    }

    pub(crate) fn send_command<C: WchLinkCommand + std::fmt::Debug>(
        &mut self,
        cmd: C,
    ) -> Result<C::Response, DebugProbeError> {
        tracing::trace!("Sending command: {:?}", cmd);

        let mut rxbuf = [0u8; 64];
        let len = cmd.to_bytes(&mut rxbuf)?;

        let timeout = Duration::from_millis(100);

        let written_bytes = self
            .device_handle
            .write_bulk(ENDPOINT_OUT, &rxbuf[..len], timeout)
            .map_err(DebugProbeError::Usb)?;

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
            .map_err(DebugProbeError::Usb)?;

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
