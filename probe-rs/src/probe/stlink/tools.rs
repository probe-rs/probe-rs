use rusb::Device;
use rusb::UsbContext;

use crate::probe::debug_probe::{DebugProbeInfo, DebugProbeType};

use super::usb_interface::USB_PID_EP_MAP;
use super::usb_interface::USB_VID;

fn is_stlink_device<T: UsbContext>(device: &Device<T>) -> bool {
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
                .map(|d| {
                    let descriptor = d
                        .device_descriptor()
                        .expect("This is a bug. Please report it.");
                    DebugProbeInfo::new(
                        "STLink ".to_owned()
                            + &USB_PID_EP_MAP[&descriptor.product_id()].version_name,
                        descriptor.vendor_id(),
                        descriptor.product_id(),
                        None,
                        DebugProbeType::STLink,
                    )
                })
                .collect::<Vec<_>>()
        } else {
            vec![]
        }
    } else {
        vec![]
    }
}
