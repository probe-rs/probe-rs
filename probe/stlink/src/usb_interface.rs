use lazy_static::lazy_static;
use libusb::{Context, Device, DeviceHandle, Error};
use std::time::Duration;

use std::collections::HashMap;

use crate::stlink::{STLinkError, ToSTLinkErr};

/// The USB Command packet size.
const CMD_LEN: usize = 16;

/// The USB VendorID.
const USB_VID: u16 = 0x0483;

pub const TIMEOUT: Duration = Duration::from_millis(1000);

lazy_static! {
    /// Map of USB PID to firmware version name and device endpoints.
    static ref USB_PID_EP_MAP: HashMap<u16, STLinkInfo> = {
        let mut m = HashMap::new();
        m.insert(0x3748, STLinkInfo::new("V2",    0x3748, 0x02,   0x81,   0x83));
        m.insert(0x374b, STLinkInfo::new("V2-1",  0x374b, 0x01,   0x81,   0x82));
        m.insert(0x374a, STLinkInfo::new("V2-1",  0x374a, 0x01,   0x81,   0x82));  // Audio
        m.insert(0x3742, STLinkInfo::new("V2-1",  0x3742, 0x01,   0x81,   0x82));  // No MSD
        m.insert(0x374e, STLinkInfo::new("V3",    0x374e, 0x01,   0x81,   0x82));
        m.insert(0x374f, STLinkInfo::new("V3",    0x374f, 0x01,   0x81,   0x82));  // Bridge
        m.insert(0x3753, STLinkInfo::new("V3",    0x3753, 0x01,   0x81,   0x82));  // 2VCP
        m
    };
}

/// A helper struct to match STLink deviceinfo.
#[derive(Clone)]
pub struct STLinkInfo {
    pub version_name: String,
    pub usb_pid: u16,
    ep_out: u8,
    ep_in: u8,
    ep_swv: u8,
}

impl STLinkInfo {
    pub fn new<V: Into<String>>(
        version_name: V,
        usb_pid: u16,
        ep_out: u8,
        ep_in: u8,
        ep_swv: u8,
    ) -> Self {
        Self {
            version_name: version_name.into(),
            usb_pid,
            ep_out,
            ep_in,
            ep_swv,
        }
    }
}

/// Provides low-level USB enumeration and transfers for STLinkV2/3 devices.
pub struct STLinkUSBDevice<'a> {
    device: Device<'a>,
    device_handle: Option<DeviceHandle<'a>>,
    pub info: STLinkInfo,
}

fn usb_match<'a>(device: &Device<'a>) -> bool {
    // Check the VID/PID.
    if let Ok(descriptor) = device.device_descriptor() {
        (descriptor.vendor_id() == USB_VID)
            && (USB_PID_EP_MAP.contains_key(&descriptor.product_id()))
    } else {
        false
    }
}

pub fn get_all_plugged_devices<'a>(
    context: &'a Context,
) -> Result<Vec<STLinkUSBDevice<'a>>, Error> {
    let devices = context.devices()?;
    devices
        .iter()
        .filter(usb_match)
        .map(|device| STLinkUSBDevice::new(device))
        .collect::<Result<Vec<_>, Error>>()
}

impl<'a> STLinkUSBDevice<'a> {
    pub fn new(device: Device<'a>) -> Result<Self, Error> {
        let descriptor = device.device_descriptor()?;
        let info = USB_PID_EP_MAP[&descriptor.product_id()].clone();
        Ok(Self {
            device,
            device_handle: None,
            info,
        })
    }

    pub fn open(&mut self) -> Result<(), STLinkError> {
        self.device_handle = Some(self.device.open().or_usb_err()?);
        // The next statement will never fail.
        self.device_handle
            .as_mut()
            .map(|ref mut dh| dh.claim_interface(0));

        let config = self.device.active_config_descriptor().or_usb_err()?;
        let descriptor = self.device.device_descriptor().or_usb_err()?;
        let info = &USB_PID_EP_MAP[&descriptor.product_id()];

        let mut endpoint_out = None;
        let mut endpoint_in = None;
        let mut endpoint_swv = None;

        if let Some(interface) = config.interfaces().next() {
            if let Some(descriptor) = interface.descriptors().next() {
                for endpoint in descriptor.endpoint_descriptors() {
                    if endpoint.address() == info.ep_out {
                        endpoint_out = Some(info.ep_out);
                    } else if endpoint.address() == info.ep_in {
                        endpoint_in = Some(info.ep_in);
                    } else if endpoint.address() == info.ep_swv {
                        endpoint_swv = Some(info.ep_swv);
                    }
                }
            }
        }

        if endpoint_out.is_none() {
            return Err(STLinkError::EndpointNotFound);
        }

        if endpoint_in.is_none() {
            return Err(STLinkError::EndpointNotFound);
        }

        if endpoint_swv.is_none() {
            return Err(STLinkError::EndpointNotFound);
        }

        //self.flush_rx();
        self.read(1000, Duration::from_millis(10))?;

        Ok(())
    }

    pub fn close(&mut self) -> Result<(), Error> {
        self.device_handle
            .as_mut()
            .map_or(Err(Error::NotFound), |dh| dh.release_interface(0))?;
        self.device_handle = None;
        Ok(())
    }

    pub fn read(&mut self, size: u16, timeout: Duration) -> Result<Vec<u8>, STLinkError> {
        let mut buf = vec![0; size as usize];
        let ep_in = self.info.ep_in;
        self.device_handle
            .as_mut()
            .map(|dh| dh.read_bulk(ep_in, buf.as_mut_slice(), timeout));
        Ok(buf)
    }

    pub fn write(
        &mut self,
        mut cmd: Vec<u8>,
        write_data: &[u8],
        read_data: &mut [u8],
        timeout: Duration,
    ) -> Result<(), STLinkError> {
        // Command phase.
        for _ in 0..(CMD_LEN - cmd.len()) {
            cmd.push(0);
        }

        let ep_out = self.info.ep_out;
        let ep_in = self.info.ep_in;

        let written_bytes = self
            .device_handle
            .as_mut()
            .map(|dh| dh.write_bulk(ep_out, &cmd, timeout))
            .unwrap()
            .or_usb_err()?;

        if written_bytes != CMD_LEN {
            return Err(STLinkError::NotEnoughBytesRead);
        }

        // Optional data out phase.
        if write_data.len() > 0 {
            let written_bytes = self
                .device_handle
                .as_mut()
                .map(|dh| dh.write_bulk(ep_out, write_data, timeout))
                .unwrap()
                .or_usb_err()?;
            if written_bytes != write_data.len() {
                return Err(STLinkError::NotEnoughBytesRead);
            }
        }

        // Optional data in phase.
        if read_data.len() > 0 {
            let read_bytes = self
                .device_handle
                .as_mut()
                .map(|dh| dh.read_bulk(ep_in, read_data, timeout))
                .unwrap()
                .or_usb_err()?;
            if read_bytes != read_data.len() {
                return Err(crate::stlink::STLinkError::NotEnoughBytesRead);
            }
        }
        Ok(())
    }

    pub fn read_swv(&mut self, size: usize, timeout: Duration) -> Result<Vec<u8>, STLinkError> {
        let ep_swv = self.info.ep_swv;
        let mut buf = Vec::with_capacity(size as usize);
        let read_bytes = self
            .device_handle
            .as_mut()
            .map(|dh| dh.read_bulk(ep_swv, buf.as_mut_slice(), timeout))
            .unwrap()
            .or_usb_err()?;
        if read_bytes != size {
            return Err(crate::stlink::STLinkError::NotEnoughBytesRead);
        } else {
            Ok(buf)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn list_devices() {}
}