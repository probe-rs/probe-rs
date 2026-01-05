use nusb::{DeviceInfo, MaybeFuture};
use std::{sync::LazyLock, time::Duration};

use crate::probe::{stlink::StlinkError, usb_util::InterfaceExt};

use std::collections::HashMap;

use super::tools::{is_stlink_device, read_serial_number};
use crate::probe::{DebugProbeSelector, ProbeCreationError};

/// The USB Command packet size.
const CMD_LEN: usize = 16;

/// The USB VendorID.
pub const USB_VID: u16 = 0x0483;

pub const TIMEOUT: Duration = Duration::from_millis(1000);

/// Map of USB PID to firmware version name and device endpoints.
pub static USB_PID_EP_MAP: LazyLock<HashMap<u16, StLinkInfo>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert(0x3748, StLinkInfo::new("V2", 0x02, 0x81, 0x83));
    m.insert(0x374b, StLinkInfo::new("V2-1", 0x01, 0x81, 0x82));
    m.insert(0x374a, StLinkInfo::new("V2-1", 0x01, 0x81, 0x82)); // Audio
    m.insert(0x3742, StLinkInfo::new("V2-1", 0x01, 0x81, 0x82)); // No MSD
    m.insert(0x3752, StLinkInfo::new("V2-1", 0x01, 0x81, 0x82)); // Unproven
    m.insert(0x374e, StLinkInfo::new("V3", 0x01, 0x81, 0x82));
    m.insert(0x374f, StLinkInfo::new("V3", 0x01, 0x81, 0x82)); // Bridge
    m.insert(0x3753, StLinkInfo::new("V3", 0x01, 0x81, 0x82)); // 2VCP
    m.insert(0x3754, StLinkInfo::new("V3", 0x01, 0x81, 0x82)); // Without mass storage
    m.insert(0x3757, StLinkInfo::new("V3PWR", 0x01, 0x81, 0x82)); // Bridge and power, no MSD
    m
});

/// A helper struct to match STLink device info.
#[derive(Clone, Debug, Default)]
pub struct StLinkInfo {
    pub version_name: &'static str,
    ep_out: u8,
    ep_in: u8,
    ep_swo: u8,
}

impl StLinkInfo {
    pub const fn new(version_name: &'static str, ep_out: u8, ep_in: u8, ep_swo: u8) -> Self {
        Self {
            version_name,
            ep_out,
            ep_in,
            ep_swo,
        }
    }
}

pub(crate) struct StLinkUsbDevice {
    device_handle: nusb::Device,
    interface: nusb::Interface,
    pub(crate) info: &'static StLinkInfo,
}

impl std::fmt::Debug for StLinkUsbDevice {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("StLinkUsbDevice")
            .field("device_handle", &"DeviceHandle<rusb::Context>")
            .field("info", &self.info)
            .finish()
    }
}

pub trait StLinkUsb: std::fmt::Debug {
    /// Writes to the probe and reads back data if needed.
    fn write(
        &mut self,
        cmd: &[u8],
        write_data: &[u8],
        read_data: &mut [u8],
        timeout: Duration,
    ) -> Result<(), StlinkError>;

    /// Reset the USB device. This can be used to recover when the
    /// STLink does not respond to USB requests.
    fn reset(&mut self) -> Result<(), StlinkError>;

    /// Reads SWO data from the probe.
    fn read_swo(&mut self, read_data: &mut [u8], timeout: Duration) -> Result<usize, StlinkError>;
}

// Copy of `Selector::matches` except it uses the stlink-specific read_serial_number
// to handle the broken stlink-v2 serial numbers that need hex-encoding.
fn selector_matches(selector: &DebugProbeSelector, info: &DeviceInfo) -> bool {
    info.vendor_id() == selector.vendor_id
        && info.product_id() == selector.product_id
        && selector
            .serial_number
            .as_ref()
            .map(|s| {
                if let Some(serial) = read_serial_number(info) {
                    serial.as_str() == s
                } else {
                    s.is_empty()
                }
            })
            .unwrap_or(true)
}

impl StLinkUsbDevice {
    /// Creates and initializes a new USB device.
    pub fn new_from_selector(selector: &DebugProbeSelector) -> Result<Self, ProbeCreationError> {
        let device = nusb::list_devices()
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?
            .filter(is_stlink_device)
            .find(|device| selector_matches(selector, device))
            .ok_or(ProbeCreationError::NotFound)?;

        let info = &USB_PID_EP_MAP[&device.product_id()];

        let device_handle = device
            .open()
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;
        tracing::debug!("Aquired handle for probe");

        let mut endpoint_out = false;
        let mut endpoint_in = false;
        let mut endpoint_swo = false;

        let Some(config) = device_handle.configurations().next() else {
            tracing::warn!("Unable to get configurations of ST-Link USB device");
            return Err(ProbeCreationError::CouldNotOpen);
        };

        if let Some(interface) = config.interfaces().next()
            && let Some(descriptor) = interface.alt_settings().next()
        {
            for endpoint in descriptor.endpoints() {
                if endpoint.address() == info.ep_out {
                    endpoint_out = true;
                } else if endpoint.address() == info.ep_in {
                    endpoint_in = true;
                } else if endpoint.address() == info.ep_swo {
                    endpoint_swo = true;
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

        let interface = device_handle
            .claim_interface(0)
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;

        tracing::debug!("Claimed interface 0 of USB device.");

        let usb_stlink = Self {
            device_handle,
            interface,
            info,
        };

        tracing::debug!("Succesfully attached to STLink.");

        Ok(usb_stlink)
    }
}

impl StLinkUsb for StLinkUsbDevice {
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
    ) -> Result<(), StlinkError> {
        tracing::trace!(
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

        let written_bytes = self.interface.write_bulk(ep_out, &padded_cmd, timeout)?;

        if written_bytes != CMD_LEN {
            return Err(StlinkError::NotEnoughBytesWritten {
                is: written_bytes,
                should: CMD_LEN,
            });
        }

        // Optional data out phase.
        if !write_data.is_empty() {
            let mut remaining_bytes = write_data.len();

            let mut write_index = 0;

            while remaining_bytes > 0 {
                let written_bytes =
                    self.interface
                        .write_bulk(ep_out, &write_data[write_index..], timeout)?;

                remaining_bytes -= written_bytes;
                write_index += written_bytes;

                tracing::trace!(
                    "Wrote {} bytes, {} bytes remaining",
                    written_bytes,
                    remaining_bytes
                );
            }

            tracing::trace!("USB write done!");
        }

        // Optional data in phase.
        if !read_data.is_empty() {
            let mut remaining_bytes = read_data.len();
            let mut read_index = 0;

            while remaining_bytes > 0 {
                let read_bytes =
                    self.interface
                        .read_bulk(ep_in, &mut read_data[read_index..], timeout)?;

                read_index += read_bytes;
                remaining_bytes -= read_bytes;

                tracing::trace!(
                    "Read {} bytes, {} bytes remaining",
                    read_bytes,
                    remaining_bytes
                );
            }
        }
        Ok(())
    }

    fn read_swo(&mut self, read_data: &mut [u8], timeout: Duration) -> Result<usize, StlinkError> {
        tracing::trace!(
            "Reading {:?} SWO bytes to STLink, timeout: {:?}",
            read_data.len(),
            timeout
        );

        let ep_swo = self.info.ep_swo;

        if read_data.is_empty() {
            Ok(0)
        } else {
            self.interface
                .read_bulk(ep_swo, read_data, timeout)
                .map_err(StlinkError::Usb)
        }
    }

    /// Reset the USB device. This can be used to recover when the
    /// STLink does not respond to USB requests.
    fn reset(&mut self) -> Result<(), StlinkError> {
        tracing::debug!("Resetting USB device of STLink");
        self.device_handle
            .reset()
            .wait()
            .map_err(|e| StlinkError::Usb(e.into()))
    }
}
