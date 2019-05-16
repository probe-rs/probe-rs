use libusb::Device;
use probe::debug_probe::{
    DebugProbeInfo,
    DebugProbeType,
};

fn is_stlink_device<'a>(device: &Device<'a>) -> bool {
    // Check the VID/PID.
    if let Ok(descriptor) = device.device_descriptor() {
        (descriptor.vendor_id() == crate::usb_interface::USB_VID)
            && (crate::usb_interface::USB_PID_EP_MAP.contains_key(&descriptor.product_id()))
    } else {
        false
    }
}

pub fn list_stlink_devices() -> Vec<DebugProbeInfo> {
    if let Ok(context) = libusb::Context::new() {
        if let Ok(devices) = context.devices() {
            devices.iter()
                    .filter(is_stlink_device)
                    .map(|d| {
                        let descriptor = d.device_descriptor().expect("This is a bug. Please report it.");
                        DebugProbeInfo::new(
                            "STLink ".to_owned() + &crate::usb_interface::USB_PID_EP_MAP[&descriptor.product_id()].version_name,
                            descriptor.vendor_id(),
                            descriptor.product_id(),
                            None,
                            DebugProbeType::STLink
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