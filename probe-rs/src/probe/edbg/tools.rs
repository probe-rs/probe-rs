use crate::probe::{DebugProbeInfo, DebugProbeType};
/// Finds all CMSIS-DAP devices, either v1 (HID) or v2 (WinUSB Bulk).
///
/// This method uses rusb to read device strings, which might fail due
/// to permission or driver errors, so it falls back to listing only
/// HID devices if it does not find any suitable devices.
pub fn list_edbg_devices() -> Vec<DebugProbeInfo> {
    log::debug!("Searching for EDBG probes using HID");

    let mut probes = vec![];
    if let Ok(api) = hidapi::HidApi::new() {
        for device in api.device_list() {
            if let Some(info) = get_edbg_hid_info(&device) {
                log::trace!("Adding new HID-only probe {:?}", info);
                probes.push(info)
            }
        }
    }

    log::debug!("Found {} EDBG probes total", probes.len());
    probes
}

/// Checks if a given HID device is a CMSIS-DAP v1 probe, returning Some(DebugProbeInfo) if so.
fn get_edbg_hid_info(device: &hidapi::DeviceInfo) -> Option<DebugProbeInfo> {
    if let Some(prod_str) = device.product_string() {
        if prod_str.contains("EDBG") {
            return Some(DebugProbeInfo {
                identifier: prod_str.to_owned(),
                vendor_id: device.vendor_id(),
                product_id: device.product_id(),
                serial_number: device.serial_number().map(|s| s.to_owned()),
                probe_type: DebugProbeType::EDBG,
                hid_interface: Some(device.interface_number() as u8),
            });
        }
    }
    None
}
