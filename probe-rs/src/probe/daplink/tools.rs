use crate::probe::{DebugProbeInfo, DebugProbeType};

pub fn list_daplink_devices() -> Vec<DebugProbeInfo> {
    match hidapi::HidApi::new() {
        Ok(api) => api
            .devices()
            .iter()
            .cloned()
            .filter(|device| is_daplink_device(&device))
            .map(|v| {
                DebugProbeInfo::new(
                    v.product_string
                        .unwrap_or_else(|| "Unknown CMSIS-DAP Probe".to_owned()),
                    v.vendor_id,
                    v.product_id,
                    v.serial_number,
                    DebugProbeType::DAPLink,
                )
            })
            .collect::<Vec<_>>(),
        Err(_e) => vec![],
    }
}

pub fn is_daplink_device(device: &hidapi::HidDeviceInfo) -> bool {
    if let Some(product_string) = device.product_string.as_ref() {
        product_string.contains("CMSIS-DAP")
    } else {
        false
    }
}

pub fn read_status(device: &mut std::sync::Mutex<hidapi::HidDevice>) {
    let vendor_id: super::commands::general::info::VendorID =
        super::commands::send_command(device, super::commands::general::info::Command::VendorID)
            .unwrap();
    log::info!("{:?}", vendor_id);
    let product_id: super::commands::general::info::ProductID =
        super::commands::send_command(device, super::commands::general::info::Command::ProductID)
            .unwrap();
    log::info!("{:?}", product_id);
    let serial_number: super::commands::general::info::SerialNumber =
        super::commands::send_command(
            device,
            super::commands::general::info::Command::SerialNumber,
        )
        .unwrap();
    log::info!("{:?}", serial_number);
    let firmware_version: super::commands::general::info::FirmwareVersion =
        super::commands::send_command(
            device,
            super::commands::general::info::Command::FirmwareVersion,
        )
        .unwrap();
    log::info!("{:?}", firmware_version);

    let target_device_vendor: super::commands::general::info::TargetDeviceVendor =
        super::commands::send_command(
            device,
            super::commands::general::info::Command::TargetDeviceVendor,
        )
        .unwrap();
    log::info!("{:?}", target_device_vendor);

    let target_device_name: super::commands::general::info::TargetDeviceName =
        super::commands::send_command(
            device,
            super::commands::general::info::Command::TargetDeviceName,
        )
        .unwrap();
    log::info!("{:?}", target_device_name);
}
