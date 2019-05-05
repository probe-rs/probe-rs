pub fn list_daplink_devices() -> Vec<hidapi::HidDeviceInfo> {
    match hidapi::HidApi::new() {
        Ok(api) => {
            api.devices()
               .clone()
               .into_iter()
               .filter(|device| is_daplink_device(&device))
               .collect::<Vec<hidapi::HidDeviceInfo>>()
        },
        Err(e) => {
            eprintln!("Error: {}", e);
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

pub fn read_status(device_info: &hidapi::HidDeviceInfo) {
    let vendor_id: crate::commands::general::info::VendorID
        = crate::commands::send_command(device_info, crate::commands::general::info::Command::VendorID).unwrap();
    println!("{:?}", vendor_id);
    let product_id: crate::commands::general::info::ProductID
        = crate::commands::send_command(device_info, crate::commands::general::info::Command::ProductID).unwrap();
    println!("{:?}", product_id);
    let serial_number: crate::commands::general::info::SerialNumber
        = crate::commands::send_command(device_info, crate::commands::general::info::Command::SerialNumber).unwrap();
    println!("{:?}", serial_number);
    let firmware_version: crate::commands::general::info::FirmwareVersion
        = crate::commands::send_command(device_info, crate::commands::general::info::Command::FirmwareVersion).unwrap();
    println!("{:?}", firmware_version);

    let target_device_vendor: crate::commands::general::info::TargetDeviceVendor
        = crate::commands::send_command(device_info, crate::commands::general::info::Command::TargetDeviceVendor).unwrap();
    println!("{:?}", target_device_vendor);

    let target_device_name: crate::commands::general::info::TargetDeviceName
        = crate::commands::send_command(device_info, crate::commands::general::info::Command::TargetDeviceName).unwrap();
    println!("{:?}", target_device_name);
}