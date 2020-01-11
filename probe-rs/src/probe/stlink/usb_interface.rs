use crate::probe::DebugProbeInfo;
use lazy_static::lazy_static;
use rusb::{Context, Device, DeviceHandle, Error, UsbContext};
use std::time::Duration;

use std::collections::HashMap;

use crate::probe::DebugProbeError;

/// The USB Command packet size.
const CMD_LEN: usize = 16;

/// The USB VendorID.
pub const USB_VID: u16 = 0x0483;

pub const TIMEOUT: Duration = Duration::from_millis(1000);

lazy_static! {
    /// Map of USB PID to firmware version name and device endpoints.
    pub static ref USB_PID_EP_MAP: HashMap<u16, STLinkInfo> = {
        let mut m = HashMap::new();
        m.insert(0x3748, STLinkInfo::new("V2",    0x3748, 0x02,   0x81,   0x83));
        m.insert(0x374b, STLinkInfo::new("V2-1",  0x374b, 0x01,   0x81,   0x82));
        m.insert(0x374a, STLinkInfo::new("V2-1",  0x374a, 0x01,   0x81,   0x82));  // Audio
        m.insert(0x3742, STLinkInfo::new("V2-1",  0x3742, 0x01,   0x81,   0x82));  // No MSD
        m.insert(0x3752, STLinkInfo::new("V2-1",  0x3752, 0x01,   0x81,   0x82));  // Unproven
        m.insert(0x374e, STLinkInfo::new("V3",    0x374e, 0x01,   0x81,   0x82));
        m.insert(0x374f, STLinkInfo::new("V3",    0x374f, 0x01,   0x81,   0x82));  // Bridge
        m.insert(0x3753, STLinkInfo::new("V3",    0x3753, 0x01,   0x81,   0x82));  // 2VCP
        m
    };
}

/// A helper struct to match STLink deviceinfo.
#[derive(Clone, Default)]
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

rental! {
    // This lint is not in our hands; disable it for this scope.
    #[allow(clippy::useless_transmute)]
    pub mod rent {
        use super::*;
        /// Provides low-level USB enumeration and transfers for STLinkV2/3 devices.
        #[rental]
        pub struct STLinkUSBDeviceRenter {
            context: Box<rusb::Context>,
            device: Box<Device<rusb::Context>>,
            device_handle: Box<DeviceHandle<rusb::Context>>,
        }
    }
}

pub use rent::STLinkUSBDeviceRenter;

pub struct STLinkUSBDevice {
    renter: STLinkUSBDeviceRenter,
    info: STLinkInfo,
}

impl STLinkUSBDevice {
    /// Creates and initializes a new USB device.
    pub fn new_from_info(probe_info: &DebugProbeInfo) -> Result<Self, DebugProbeError> {
        let context = Context::new().map_err(|_| DebugProbeError::USBError)?;

        let mut info = Default::default();

        let renter = STLinkUSBDeviceRenter::try_new(
            Box::new(context),
            |context| {
                Ok(Box::new(
                    context
                        .devices()
                        .map_err(|_| DebugProbeError::ProbeCouldNotBeCreated)?
                        .iter()
                        .find(|device| {
                            if let Ok(descriptor) = device.device_descriptor() {
                                probe_info.vendor_id == descriptor.vendor_id()
                                    && probe_info.product_id == descriptor.product_id()
                            } else {
                                false
                            }
                        })
                        .map_or(Err(DebugProbeError::ProbeCouldNotBeCreated), Ok)?,
                ))
            },
            |device, _context| {
                let mut device_handle =
                    Box::new(device.open().map_err(|_| DebugProbeError::USBError)?);

                let config = device
                    .active_config_descriptor()
                    .map_err(|_| DebugProbeError::USBError)?;
                let descriptor = device
                    .device_descriptor()
                    .map_err(|_| DebugProbeError::USBError)?;
                info = USB_PID_EP_MAP[&descriptor.product_id()].clone();

                device_handle
                    .claim_interface(0)
                    .map_err(|_| DebugProbeError::USBError)?;

                let mut endpoint_out = false;
                let mut endpoint_in = false;
                let mut endpoint_swv = false;

                if let Some(interface) = config.interfaces().next() {
                    if let Some(descriptor) = interface.descriptors().next() {
                        for endpoint in descriptor.endpoint_descriptors() {
                            if endpoint.address() == info.ep_out {
                                endpoint_out = true;
                            } else if endpoint.address() == info.ep_in {
                                endpoint_in = true;
                            } else if endpoint.address() == info.ep_swv {
                                endpoint_swv = true;
                            }
                        }
                    }
                }

                if !endpoint_out {
                    return Err(DebugProbeError::EndpointNotFound);
                }

                if !endpoint_in {
                    return Err(DebugProbeError::EndpointNotFound);
                }

                if !endpoint_swv {
                    return Err(DebugProbeError::EndpointNotFound);
                }

                Ok(device_handle)
            },
        )
        .or_else(|_| Err(DebugProbeError::RentalInitError))?;

        let usb_stlink = Self { renter, info };

        Ok(usb_stlink)
    }

    /// Writes to the out EP.
    pub fn read(&mut self, size: u16, timeout: Duration) -> Result<Vec<u8>, DebugProbeError> {
        let mut buf = vec![0; size as usize];
        let ep_in = self.info.ep_in;
        self.renter
            .rent(|dh| dh.read_bulk(ep_in, buf.as_mut_slice(), timeout))
            .map_err(|_| DebugProbeError::USBError)?;
        Ok(buf)
    }

    /// Writes to the out EP and reads back data if needed.
    /// First the `cmd` is sent.
    /// In a second step `write_data` is transmitted.
    /// And lastly, data will be read back until `read_data` is filled.
    pub fn write(
        &mut self,
        mut cmd: Vec<u8>,
        write_data: &[u8],
        read_data: &mut [u8],
        timeout: Duration,
    ) -> Result<(), DebugProbeError> {
        // Command phase.
        for _ in 0..(CMD_LEN - cmd.len()) {
            cmd.push(0);
        }

        let ep_out = self.info.ep_out;
        let ep_in = self.info.ep_in;

        let written_bytes = self
            .renter
            .rent(|dh| dh.write_bulk(ep_out, &cmd, timeout))
            .map_err(|_| DebugProbeError::USBError)?;

        if written_bytes != CMD_LEN {
            return Err(DebugProbeError::NotEnoughBytesRead);
        }
        // Optional data out phase.
        if !write_data.is_empty() {
            let written_bytes = self
                .renter
                .rent(|dh| dh.write_bulk(ep_out, write_data, timeout))
                .map_err(|_| DebugProbeError::USBError)?;
            if written_bytes != write_data.len() {
                return Err(DebugProbeError::NotEnoughBytesRead);
            }
        }
        // Optional data in phase.
        if !read_data.is_empty() {
            let read_bytes = self
                .renter
                .rent(|dh| dh.read_bulk(ep_in, read_data, timeout))
                .map_err(|_| DebugProbeError::USBError)?;
            if read_bytes != read_data.len() {
                return Err(DebugProbeError::NotEnoughBytesRead);
            }
        }
        Ok(())
    }

    /// Special read, TODO: for later.
    pub fn read_swv(&mut self, size: usize, timeout: Duration) -> Result<Vec<u8>, DebugProbeError> {
        let ep_swv = self.info.ep_swv;
        let mut buf = Vec::with_capacity(size as usize);
        let read_bytes = self
            .renter
            .rent(|dh| dh.read_bulk(ep_swv, buf.as_mut_slice(), timeout))
            .map_err(|_| DebugProbeError::USBError)?;
        if read_bytes != size {
            Err(DebugProbeError::NotEnoughBytesRead)
        } else {
            Ok(buf)
        }
    }

    /// Closes the USB interface gracefully.
    /// Internal helper.
    fn close(&mut self) -> Result<(), Error> {
        self.renter.rent_mut(|dh| dh.release_interface(0))
    }
}

impl Drop for STLinkUSBDevice {
    fn drop(&mut self) {
        // We ignore the error case as we can't do much about it anyways.
        let _ = self.close();
    }
}
