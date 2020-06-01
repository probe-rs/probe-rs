use rusb::Device;
use rusb::UsbContext;

use crate::probe::{DebugProbeInfo, DebugProbeType};

use super::usb_interface::USB_PID_EP_MAP;
use super::usb_interface::USB_VID;
use std::time::Duration;

pub(super) fn is_stlink_device<T: UsbContext>(device: &Device<T>) -> bool {
    // Check the VID/PID.
    if let Ok(descriptor) = device.device_descriptor() {
        (descriptor.vendor_id() == USB_VID)
            && (USB_PID_EP_MAP.contains_key(&descriptor.product_id()))
    } else {
        false
    }
}

pub fn list_stlink_devices() -> Vec<DebugProbeInfo> {
    if let Ok(context) = rusb::Context::new() {
        if let Ok(devices) = context.devices() {
            devices
                .iter()
                .filter(is_stlink_device)
                .filter_map(|device| {
                    let timeout = Duration::from_millis(100);
                    let descriptor = device.device_descriptor().ok()?;
                    let handle = device.open().ok()?;
                    let language = handle.read_languages(timeout).ok()?[0];
                    let sn_str = handle
                        .read_serial_number_string(language, &descriptor, timeout)
                        .ok();
                    Some(DebugProbeInfo::new(
                        format!(
                            "STLink {}",
                            &USB_PID_EP_MAP[&descriptor.product_id()].version_name
                        ),
                        descriptor.vendor_id(),
                        descriptor.product_id(),
                        sn_str,
                        DebugProbeType::STLink,
                    ))
                })
                .collect::<Vec<_>>()
        } else {
            vec![]
        }
    } else {
        vec![]
    }
}
