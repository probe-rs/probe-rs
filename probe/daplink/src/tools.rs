use probe::debug_probe::{
    DebugProbeInfo,
    DebugProbeType,
};

pub fn list_daplink_devices() -> Vec<DebugProbeInfo> {
    match hidapi::HidApi::new() {
        Ok(api) => {
            api.devices()
               .iter()
               .cloned()
               .filter(|device| is_daplink_device(&device))
                .map(|v| DebugProbeInfo::new(
                    v.product_string.unwrap_or("Unknown CMSIS-DAP Probe".to_owned()),
                    v.vendor_id,
                    v.product_id,
                    v.serial_number.map(|v| v.to_owned()),
                    DebugProbeType::DAPLink
                ))
               .collect::<Vec<_>>()
        },
        Err(e) => {
            vec![]
        },
    }
}

pub fn is_daplink_device(device: &hidapi::HidDeviceInfo) -> bool {
    if let Some(product_string) = device.product_string.as_ref() {
        product_string.contains("CMSIS-DAP")
    } else {
        false
    }
}

pub fn read_status(device: &hidapi::HidDevice) {
    let vendor_id: crate::commands::general::info::VendorID
        = crate::commands::send_command(device, crate::commands::general::info::Command::VendorID).unwrap();
    println!("{:?}", vendor_id);
    let product_id: crate::commands::general::info::ProductID
        = crate::commands::send_command(device, crate::commands::general::info::Command::ProductID).unwrap();
    println!("{:?}", product_id);
    let serial_number: crate::commands::general::info::SerialNumber
        = crate::commands::send_command(device, crate::commands::general::info::Command::SerialNumber).unwrap();
    println!("{:?}", serial_number);
    let firmware_version: crate::commands::general::info::FirmwareVersion
        = crate::commands::send_command(device, crate::commands::general::info::Command::FirmwareVersion).unwrap();
    println!("{:?}", firmware_version);

    let target_device_vendor: crate::commands::general::info::TargetDeviceVendor
        = crate::commands::send_command(device, crate::commands::general::info::Command::TargetDeviceVendor).unwrap();
    println!("{:?}", target_device_vendor);

    let target_device_name: crate::commands::general::info::TargetDeviceName
        = crate::commands::send_command(device, crate::commands::general::info::Command::TargetDeviceName).unwrap();
    println!("{:?}", target_device_name);
}