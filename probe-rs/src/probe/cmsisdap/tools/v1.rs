//! CMSIS-DAP v1 probes, which use HID.

use super::{is_cmsis_dap, reject_probe_by_version};
use crate::probe::{
    DebugProbeInfo, DebugProbeSelector, ProbeCreationError,
    cmsisdap::{CmsisDapDevice, CmsisDapFactory, commands::DEFAULT_USB_TIMEOUT},
    list::ProbeListItem,
};
use hidapi::HidApi;

/// Adds the HID probes that are not in `probes` already.
pub(super) fn list_hid_only_devices(probes: &mut Vec<ProbeListItem>) {
    tracing::debug!(
        "Found {} CMSIS-DAP probes using nusb, searching HID",
        probes.len()
    );

    let Ok(api) = HidApi::new() else {
        return;
    };

    for info in api.device_list().filter_map(get_cmsisdap_hid_info) {
        let duplicate = probes.iter().any(|p| {
            p.info.vendor_id == info.vendor_id
                && p.info.product_id == info.product_id
                && p.info.serial_number.as_deref().unwrap_or("")
                    == info.serial_number.as_deref().unwrap_or("")
        });
        if duplicate {
            tracing::trace!("Ignoring duplicate {:?}", info);
        } else {
            tracing::trace!("Adding new HID-only probe {:?}", info);
            // HID-only probes are not opened over usbfs, so the access check doesn't apply.
            probes.push(ProbeListItem::accessible(info));
        }
    }
}

/// Checks if a given HID device is a CMSIS-DAP v1 probe, returning Some(DebugProbeInfo) if so.
fn get_cmsisdap_hid_info(device: &hidapi::DeviceInfo) -> Option<DebugProbeInfo> {
    let prod_str = device.product_string().unwrap_or("");
    let path = device.path().to_str().unwrap_or("");
    if is_cmsis_dap(prod_str) || is_cmsis_dap(path) {
        tracing::trace!("CMSIS-DAP device with USB path: {:?}", device.path());
        tracing::trace!("                product_string: {:?}", prod_str);
        tracing::trace!(
            "                     interface: {}",
            device.interface_number()
        );

        Some(DebugProbeInfo::new(
            prod_str.to_owned(),
            device.vendor_id(),
            device.product_id(),
            device.serial_number().map(|s| s.to_owned()),
            &CmsisDapFactory,
            Some(device.interface_number() as u8),
            true,
        ))
    } else {
        None
    }
}

/// Attempt to open the device matching `selector` in CMSIS-DAP v1 mode.
///
/// `hid_device_info` is the probe as nusb listed it, if it did. It picks the HID interface when
/// the probe has several.
pub(super) fn open_device(
    selector: &DebugProbeSelector,
    hid_device_info: Option<DebugProbeInfo>,
) -> Result<(CmsisDapDevice, Option<String>), ProbeCreationError> {
    // If nusb failed or the device didn't support v2, try using hidapi to open in v1 mode.
    let vid = selector.vendor_id;
    let pid = selector.product_id;
    let sn = selector.serial_number.as_deref();

    tracing::debug!(
        "Attempting to open {:04x}:{:04x} in CMSIS-DAP v1 mode",
        vid,
        pid
    );

    // Attempt to open provided VID/PID/SN with hidapi

    let Ok(hid_api) = HidApi::new() else {
        return Err(ProbeCreationError::NotFound);
    };

    let mut device_list = hid_api.device_list();

    // We have to filter manually so that we can check the correct HID interface number.
    // Using HidApi::open() will return the first device which matches PID and VID,
    // which is not always what we want.
    let device_info = device_list
        .find(|info| {
            let mut device_match = info.vendor_id() == vid && info.product_id() == pid;

            if let Some(sn) = sn {
                device_match &= Some(sn) == info.serial_number();
            }

            if let Some(hid_interface) = hid_device_info
                .as_ref()
                .and_then(|info| info.interface.filter(|_| info.is_hid_interface))
            {
                device_match &= info.interface_number() == hid_interface as i32;
            }

            device_match
        })
        .ok_or(ProbeCreationError::NotFound)?;

    let Ok(device) = device_info.open_device(&hid_api) else {
        return Err(ProbeCreationError::NotFound);
    };

    match device.get_product_string() {
        Ok(Some(s)) if is_cmsis_dap(&s) => {
            reject_probe_by_version(
                device_info.vendor_id(),
                device_info.product_id(),
                device_info.release_number(),
            )?;
            Ok((
                CmsisDapDevice::V1 {
                    handle: device,
                    report_size: hid_report_size(device_info),
                    usb_timeout: DEFAULT_USB_TIMEOUT,
                },
                Some(s),
            ))
        }
        _ => {
            // Return NotFound if this VID:PID was not a valid CMSIS-DAP probe,
            // or if it couldn't be opened, so that other probe modules can
            // attempt to open it instead.
            Err(ProbeCreationError::NotFound)
        }
    }
}

/// Manual override of HID report size
///
/// This is only needed for devices which:
///
/// 1. Don't use the default 64 bytes, and
/// 2. Don't respond to being asked the report size until they've received a full packet,
///    causing a long delay at startup as many 64-byte packets must be sent with a timeout
///    between each one.
///
/// Devices not on this list will still work but with a slow startup time
/// as the packet size is auto-determined.
fn hid_report_size(device: &hidapi::DeviceInfo) -> usize {
    // EDBG are 512-bytes and don't respond until you give them 512 bytes.
    // The nEDBG on the Curiosity Nano boards also says "EDBG" in its
    // product string ("nEDBG CMSIS-DAP"), but its reports are the usual
    // 64 bytes.
    if device.vendor_id() == 0x03eb
        && let Some(s) = device.product_string()
        && s.starts_with("EDBG")
    {
        tracing::debug!("Overriding packet size to 512 bytes for EDBG device");
        return 512;
    }

    // Default for almost all CMSIS-DAPv1 devices.
    // Devices are queried at startup for their packet size so it can usually be increased quickly
    // if needed; only some devices are annoying to query the packet size so are worth an override.
    64
}
