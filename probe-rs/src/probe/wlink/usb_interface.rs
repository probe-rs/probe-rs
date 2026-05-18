use std::time::Duration;

use nusb::{Interface, MaybeFuture, descriptors::TransferType, transfer::Direction};

use crate::probe::{
    DebugProbeError, DebugProbeSelector, ProbeCreationError, usb_util::InterfaceExt,
};

use super::{WchLinkError, commands::WchLinkCommand, get_wlink_info};

const ENDPOINT_OUT: u8 = 0x01;
const ENDPOINT_IN: u8 = 0x81;

const DATA_ENDPOINT_OUT: u8 = 0x02;
const DATA_ENDPOINT_IN: u8 = 0x82;

pub struct WchLinkUsbDevice {
    device_handle: Interface,
    data_ep_out: u8,
    data_ep_in: u8,
}

impl WchLinkUsbDevice {
    pub fn new_from_selector(selector: &DebugProbeSelector) -> Result<Self, ProbeCreationError> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;
        let device = devices
            .filter(|device| selector.matches(device))
            .find(|device| get_wlink_info(device).is_some())
            .ok_or(ProbeCreationError::NotFound)?;

        let device = device
            .open()
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;

        let configuration = device
            .configurations()
            .next()
            .ok_or(ProbeCreationError::NotFound)?;

        let interface = configuration
            .interfaces()
            .find(|intf| intf.interface_number() == 0)
            .ok_or(ProbeCreationError::NotFound)?;

        let altsetting = interface
            .alt_settings()
            .next()
            .ok_or(ProbeCreationError::NotFound)?;

        let mut endpoint_out = None;
        let mut endpoint_in = None;
        let mut data_ep_out = None;
        let mut data_ep_in = None;
        for endpoint in altsetting.endpoints() {
            if endpoint.transfer_type() != TransferType::Bulk {
                continue;
            }

            match endpoint.direction() {
                Direction::Out if endpoint.address() == ENDPOINT_OUT => {
                    endpoint_out = Some(endpoint.address());
                }
                Direction::In if endpoint.address() == ENDPOINT_IN => {
                    endpoint_in = Some(endpoint.address());
                }
                Direction::Out if endpoint.address() == DATA_ENDPOINT_OUT => {
                    data_ep_out = Some(endpoint.address());
                }
                Direction::In if endpoint.address() == DATA_ENDPOINT_IN => {
                    data_ep_in = Some(endpoint.address());
                }
                _ => {}
            }
        }

        if endpoint_out.is_none() || endpoint_in.is_none() {
            return Err(WchLinkError::EndpointNotFound.into());
        }

        let data_ep_out = data_ep_out.unwrap_or(DATA_ENDPOINT_OUT);
        let data_ep_in = data_ep_in.unwrap_or(DATA_ENDPOINT_IN);

        tracing::trace!("Acquired handle for probe");
        let device_handle = device
            .claim_interface(interface.interface_number())
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;
        tracing::trace!("Claimed interface 0 of USB device.");

        let usb_wlink = Self {
            device_handle,
            data_ep_out,
            data_ep_in,
        };

        tracing::debug!("Successfully attached to WCH-Link.");

        Ok(usb_wlink)
    }

    pub(crate) fn send_command<C: WchLinkCommand + std::fmt::Debug>(
        &mut self,
        cmd: C,
    ) -> Result<C::Response, DebugProbeError> {
        self.send_command_with_timeout(cmd, Duration::from_millis(100))
    }

    /// Send a command with a custom timeout. Used for flash operations that may
    /// take longer (e.g., EraseFlash can take several seconds).
    pub(crate) fn send_command_with_timeout<C: WchLinkCommand + std::fmt::Debug>(
        &mut self,
        cmd: C,
        timeout: Duration,
    ) -> Result<C::Response, DebugProbeError> {
        tracing::trace!("Sending command: {:?}", cmd);

        let mut rxbuf = [0u8; 64];
        let len = cmd.to_bytes(&mut rxbuf)?;

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

    /// Write data to the probe via the data endpoint (0x02 OUT).
    /// Used for sending flash algorithm binary and firmware data.
    pub(crate) fn write_data_endpoint(
        &mut self,
        buf: &[u8],
        packet_size: usize,
    ) -> Result<(), DebugProbeError> {
        let timeout = Duration::from_secs(10);

        for chunk in buf.chunks(packet_size) {
            let mut padded = chunk.to_vec();
            if padded.len() < packet_size {
                padded.resize(packet_size, 0xff);
            }
            let written = self
                .device_handle
                .write_bulk(self.data_ep_out, &padded, timeout)
                .map_err(DebugProbeError::Usb)?;
            if written != padded.len() {
                return Err(WchLinkError::NotEnoughBytesWritten {
                    is: written,
                    should: padded.len(),
                }
                .into());
            }
        }
        Ok(())
    }

    /// Read data from the probe via the data endpoint (0x82 IN).
    /// Used for receiving data during flash verify or memory read.
    pub(crate) fn read_data_endpoint(&mut self, len: usize) -> Result<Vec<u8>, DebugProbeError> {
        let timeout = Duration::from_secs(10);
        let mut buf = vec![0u8; len];
        let mut total = 0;
        while total < len {
            let chunk_size = std::cmp::min(64, len - total);
            let n = self
                .device_handle
                .read_bulk(
                    self.data_ep_in,
                    &mut buf[total..total + chunk_size],
                    timeout,
                )
                .map_err(DebugProbeError::Usb)?;
            total += n;
        }
        buf.truncate(total);
        Ok(buf)
    }
}
