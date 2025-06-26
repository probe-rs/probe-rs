use super::usb_interface::{USB_PID_EP_MAP, USB_VID};
use crate::probe::{DebugProbeInfo, DebugProbeKind, UsbFilters, stlink::StLinkFactory};
use std::fmt::Write;

pub(super) fn is_stlink_device(device: &nusb::DeviceInfo) -> bool {
    // Check the VID/PID.
    (device.vendor_id() == USB_VID) && (USB_PID_EP_MAP.contains_key(&device.product_id()))
}

#[tracing::instrument(skip_all)]
pub(super) fn list_stlink_devices() -> Vec<DebugProbeInfo> {
    let devices = match nusb::list_devices() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("listing stlink devices failed: {:?}", e);
            return vec![];
        }
    };

    devices
        .filter(is_stlink_device)
        .map(|device| {
            DebugProbeInfo::new(
                format!(
                    "STLink {}",
                    &USB_PID_EP_MAP[&device.product_id()].version_name
                ),
                DebugProbeKind::Usb {
                    vendor_id: device.vendor_id(),
                    product_id: device.product_id(),
                    filters: UsbFilters {
                        serial_number: read_serial_number(&device),
                        hid_interface: None,

                        #[cfg(any(target_os = "linux", target_os = "android"))]
                        sysfs_path: Some(device.sysfs_path().to_owned()),

                        #[cfg(target_os = "windows")]
                        instance_id: Some(device.instance_id().display().to_string()),
                        #[cfg(target_os = "windows")]
                        parent_instance_id: Some(device.parent_instance_id().display().to_string()),
                        #[cfg(target_os = "windows")]
                        port_number: Some(device.port_number()),
                        #[cfg(target_os = "windows")]
                        driver: device.driver().map(str::to_string),

                        #[cfg(target_os = "macos")]
                        registry_id: Some(device.registry_entry_id()),
                        #[cfg(target_os = "macos")]
                        location_id: Some(device.location_id()),
                    },
                },
                &StLinkFactory,
            )
        })
        .collect()
}

/// Try to read the serial number of a USB device.
pub(super) fn read_serial_number(device: &nusb::DeviceInfo) -> Option<String> {
    device.serial_number().map(|s| {
        if s.len() < 24 {
            // Some STLink (especially V2) have their serial number stored as a 12 bytes binary string
            // containing non printable characters, so convert to a hex string to make them printable.
            s.as_bytes().iter().fold(String::new(), |mut s, b| {
                let _ = write!(s, "{b:02X}"); // Writing a String never fails
                s
            })
        } else {
            // Other STlink (especially V2-1) have their serial number already stored as a 24 characters
            // hex string so they don't need to be converted
            s.to_string()
        }
    })
}
