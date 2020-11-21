use lazy_static::lazy_static;
use rusb::{Context, DeviceHandle, Error, UsbContext};
use std::time::Duration;

use crate::probe::stlink::StlinkError;

use std::collections::HashMap;

use super::tools::{is_stlink_device, read_serial_number};
use crate::{
    probe::{DebugProbeError, ProbeCreationError},
    DebugProbeSelector,
};

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
#[derive(Clone, Debug, Default)]
pub struct STLinkInfo {
    pub version_name: String,
    pub usb_pid: u16,
    ep_out: u8,
    ep_in: u8,
    ep_swo: u8,
}

impl STLinkInfo {
    pub fn new<V: Into<String>>(
        version_name: V,
        usb_pid: u16,
        ep_out: u8,
        ep_in: u8,
        ep_swo: u8,
    ) -> Self {
        Self {
            version_name: version_name.into(),
            usb_pid,
            ep_out,
            ep_in,
            ep_swo,
        }
    }
}

pub(crate) struct STLinkUSBDevice {
    device_handle: DeviceHandle<rusb::Context>,
    info: STLinkInfo,
}

impl std::fmt::Debug for STLinkUSBDevice {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("STLinkUSBDevice")
            .field("device_handle", &"DeviceHandle<rusb::Context>")
            .field("info", &self.info)
            .finish()
    }
}

pub trait StLinkUsb: std::fmt::Debug {
    fn write(
        &mut self,
        cmd: &[u8],
        write_data: &[u8],
        read_data: &mut [u8],
        timeout: Duration,
    ) -> Result<(), DebugProbeError>;

    /// Reset the USB device. This can be used to recover when the
    /// STLink does not respond to USB requests.
    fn reset(&mut self) -> Result<(), DebugProbeError>;

    fn read_swo(
        &mut self,
        read_data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, DebugProbeError>;
}

impl STLinkUSBDevice {
    /// Creates and initializes a new USB device.
    pub fn new_from_selector(
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Self, ProbeCreationError> {
        let selector = selector.into();

        let context = Context::new()?;

        log::debug!("Acquired libusb context.");

        let device = context
            .devices()?
            .iter()
            .filter(is_stlink_device)
            .find_map(|device| {
                let descriptor = device.device_descriptor().ok()?;
                // First match the VID & PID.
                if selector.vendor_id == descriptor.vendor_id()
                    && selector.product_id == descriptor.product_id()
                {
                    // If the VID & PID match, match the serial if one was given.
                    if let Some(serial) = &selector.serial_number {
                        let sn_str = read_serial_number(&device, &descriptor).ok();
                        if sn_str.as_ref() == Some(serial) {
                            Some(device)
                        } else {
                            None
                        }
                    } else {
                        // If no serial was given, the VID & PID match is enough; return the device.
                        Some(device)
                    }
                } else {
                    None
                }
            })
            .map_or(Err(ProbeCreationError::NotFound), Ok)?;

        let mut device_handle = device.open()?;

        log::debug!("Aquired handle for probe");

        let config = device.active_config_descriptor()?;

        log::debug!("Active config descriptor: {:?}", &config);

        let descriptor = device.device_descriptor()?;

        log::debug!("Device descriptor: {:?}", &descriptor);

        let info = USB_PID_EP_MAP[&descriptor.product_id()].clone();

        device_handle.claim_interface(0)?;

        log::debug!("Claimed interface 0 of USB device.");

        let mut endpoint_out = false;
        let mut endpoint_in = false;
        let mut endpoint_swo = false;

        if let Some(interface) = config.interfaces().next() {
            if let Some(descriptor) = interface.descriptors().next() {
                for endpoint in descriptor.endpoint_descriptors() {
                    if endpoint.address() == info.ep_out {
                        endpoint_out = true;
                    } else if endpoint.address() == info.ep_in {
                        endpoint_in = true;
                    } else if endpoint.address() == info.ep_swo {
                        endpoint_swo = true;
                    }
                }
            }
        }

        if !endpoint_out {
            return Err(StlinkError::EndpointNotFound.into());
        }

        if !endpoint_in {
            return Err(StlinkError::EndpointNotFound.into());
        }

        if !endpoint_swo {
            return Err(StlinkError::EndpointNotFound.into());
        }

        let usb_stlink = Self {
            device_handle,
            info,
        };

        log::debug!("Succesfully attached to STLink.");

        Ok(usb_stlink)
    }

    /// Closes the USB interface gracefully.
    /// Internal helper.
    fn close(&mut self) -> Result<(), Error> {
        self.device_handle.release_interface(0)
    }
}

impl StLinkUsb for STLinkUSBDevice {
    /// Writes to the out EP and reads back data if needed.
    /// First the `cmd` is sent.
    /// In a second step `write_data` is transmitted.
    /// And lastly, data will be read back until `read_data` is filled.
    fn write(
        &mut self,
        cmd: &[u8],
        write_data: &[u8],
        read_data: &mut [u8],
        timeout: Duration,
    ) -> Result<(), DebugProbeError> {
        log::trace!(
            "Sending command {:x?} to STLink, timeout: {:?}",
            cmd,
            timeout
        );

        // Command phase.
        assert!(cmd.len() <= CMD_LEN);
        let mut padded_cmd = [0u8; CMD_LEN];
        padded_cmd[..cmd.len()].copy_from_slice(cmd);

        let ep_out = self.info.ep_out;
        let ep_in = self.info.ep_in;

        let written_bytes = self
            .device_handle
            .write_bulk(ep_out, &padded_cmd, timeout)
            .map_err(|e| DebugProbeError::USB(Some(Box::new(e))))?;

        if written_bytes != CMD_LEN {
            return Err(StlinkError::NotEnoughBytesWritten {
                is: written_bytes,
                should: CMD_LEN,
            }
            .into());
        }

        // Optional data out phase.
        if !write_data.is_empty() {
            let mut remaining_bytes = write_data.len();

            let mut write_index = 0;

            while remaining_bytes > 0 {
                let written_bytes = self
                    .device_handle
                    .write_bulk(ep_out, &write_data[write_index..], timeout)
                    .map_err(|e| DebugProbeError::USB(Some(Box::new(e))))?;

                remaining_bytes -= written_bytes;
                write_index += written_bytes;

                log::trace!(
                    "Wrote {} bytes, {} bytes remaining",
                    written_bytes,
                    remaining_bytes
                );
            }

            log::trace!("USB write done!");
        }

        // Optional data in phase.
        if !read_data.is_empty() {
            let mut remaining_bytes = read_data.len();
            let mut read_index = 0;

            while remaining_bytes > 0 {
                let read_bytes = self
                    .device_handle
                    .read_bulk(ep_in, &mut read_data[read_index..], timeout)
                    .map_err(|e| DebugProbeError::USB(Some(Box::new(e))))?;

                read_index += read_bytes;
                remaining_bytes -= read_bytes;

                log::trace!(
                    "Read {} bytes, {} bytes remaining",
                    read_bytes,
                    remaining_bytes
                );
            }
        }
        Ok(())
    }

    fn read_swo(
        &mut self,
        read_data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, DebugProbeError> {
        log::trace!(
            "Reading {:?} SWO bytes to STLink, timeout: {:?}",
            read_data.len(),
            timeout
        );

        let ep_swo = self.info.ep_swo;

        if read_data.is_empty() {
            Ok(0)
        } else {
            let read_bytes = self
                .device_handle
                .read_bulk(ep_swo, read_data, timeout)
                .map_err(|e| DebugProbeError::USB(Some(Box::new(e))))?;
            Ok(read_bytes)
        }
    }

    /// Reset the USB device. This can be used to recover when the
    /// STLink does not respond to USB requests.
    fn reset(&mut self) -> Result<(), DebugProbeError> {
        log::debug!("Resetting USB device of STLink");
        self.device_handle
            .reset()
            .map_err(|e| DebugProbeError::USB(Some(Box::new(e))))
    }
}

impl Drop for STLinkUSBDevice {
    fn drop(&mut self) {
        // We ignore the error case as we can't do much about it anyways.
        let _ = self.close();
    }
}
