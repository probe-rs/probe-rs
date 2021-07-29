use super::CmsisDapDevice;
use crate::{
    probe::{DebugProbeInfo, DebugProbeType, ProbeCreationError},
    DebugProbeSelector,
};
use hidapi::HidApi;
use rusb::{constants::LIBUSB_CLASS_HID, Device, DeviceDescriptor, UsbContext};
use std::time::Duration;

/// Finds all CMSIS-DAP devices, either v1 (HID) or v2 (WinUSB Bulk).
///
/// This method uses rusb to read device strings, which might fail due
/// to permission or driver errors, so it falls back to listing only
/// HID devices if it does not find any suitable devices.
pub fn list_cmsisdap_devices() -> Vec<DebugProbeInfo> {
    log::debug!("Searching for CMSIS-DAP probes using libusb");
    let mut probes = match rusb::Context::new().and_then(|ctx| ctx.devices()) {
        Ok(devices) => devices
            .iter()
            .filter_map(|device| get_cmsisdap_info(&device))
            .collect(),
        Err(_) => vec![],
    };

    log::debug!(
        "Found {} CMSIS-DAP probes using libusb, searching HID",
        probes.len()
    );

    if let Ok(api) = hidapi::HidApi::new() {
        for device in api.device_list() {
            if let Some(info) = get_cmsisdap_hid_info(&device) {
                if !probes
                    .iter()
                    .any(|p| p.vendor_id == info.vendor_id && p.product_id == info.product_id)
                {
                    log::trace!("Adding new HID-only probe {:?}", info);
                    probes.push(info)
                } else {
                    log::trace!("Ignoring duplicate {:?}", info);
                }
            }
        }
    }

    log::debug!("Found {} CMSIS-DAP probes total", probes.len());
    probes
}

/// Checks if a given Device is a CMSIS-DAP probe, returning Some(DebugProbeInfo) if so.
fn get_cmsisdap_info(device: &Device<rusb::Context>) -> Option<DebugProbeInfo> {
    // Open device handle and read basic information
    let timeout = Duration::from_millis(100);
    let d_desc = device.device_descriptor().ok()?;
    let handle = device.open().ok()?;
    let language = handle.read_languages(timeout).ok()?.get(0).cloned()?;
    let prod_str = handle
        .read_product_string(language, &d_desc, timeout)
        .ok()?;
    let sn_str = handle
        .read_serial_number_string(language, &d_desc, timeout)
        .ok();

    // All CMSIS-DAP probes must have "CMSIS-DAP" in their product string.
    if prod_str.contains("CMSIS-DAP") {
        let config_descriptor = device.active_config_descriptor().ok()?;

        log::trace!(
            "{}: CMSIS-DAP device with {} interfaces",
            prod_str,
            config_descriptor.num_interfaces()
        );

        let mut cmsis_dap_interface = None;

        'interface_loop: for interface in config_descriptor.interfaces() {
            for descriptor in interface.descriptors() {
                // Check if this is a HID interface
                if descriptor.class_code() != LIBUSB_CLASS_HID {
                    log::trace!("Interface {} is not HID, skipping", interface.number());
                    continue;
                }

                let interface_desc =
                    match handle.read_interface_string(language, &descriptor, timeout) {
                        Ok(desc) => desc,
                        Err(_) => {
                            log::trace!(
                                "Could not read string for interface {}, skipping",
                                interface.number()
                            );
                            continue;
                        }
                    };

                log::trace!("  Interface {}: {}", interface.number(), interface_desc);

                if interface_desc.contains("CMSIS-DAP") {
                    cmsis_dap_interface = Some(interface.number());
                    break 'interface_loop;
                }
            }
        }

        if let Some(interface) = cmsis_dap_interface {
            log::trace!("Will use interface number {} for CMSIS-DAPv1", interface);
        } else {
            log::trace!("No HID interface for CMSIS-DAP found.")
        }

        Some(DebugProbeInfo {
            identifier: prod_str,
            vendor_id: d_desc.vendor_id(),
            product_id: d_desc.product_id(),
            serial_number: sn_str,
            probe_type: DebugProbeType::CmsisDap,
            hid_interface: cmsis_dap_interface,
        })
    } else {
        None
    }
}

/// Checks if a given HID device is a CMSIS-DAP v1 probe, returning Some(DebugProbeInfo) if so.
fn get_cmsisdap_hid_info(device: &hidapi::DeviceInfo) -> Option<DebugProbeInfo> {
    if let Some(prod_str) = device.product_string() {
        if prod_str.contains("CMSIS-DAP") {
            log::trace!("CMSIS-DAP device with USB path: {:?}", device.path());
            log::trace!("                product_string: {:?}", prod_str);
            log::trace!(
                "                     interface: {}",
                device.interface_number()
            );

            return Some(DebugProbeInfo {
                identifier: prod_str.to_owned(),
                vendor_id: device.vendor_id(),
                product_id: device.product_id(),
                serial_number: device.serial_number().map(|s| s.to_owned()),
                probe_type: DebugProbeType::CmsisDap,
                hid_interface: Some(device.interface_number() as u8),
            });
        }
    }
    None
}

/// Attempt to open the given device in CMSIS-DAP v2 mode
pub fn open_v2_device(device: Device<rusb::Context>) -> Option<CmsisDapDevice> {
    // Open device handle and read basic information
    let timeout = Duration::from_millis(100);
    let d_desc = device.device_descriptor().ok()?;
    let vid = d_desc.vendor_id();
    let pid = d_desc.product_id();
    let mut handle = device.open().ok()?;
    let language = handle.read_languages(timeout).ok()?.get(0).cloned()?;

    // Go through interfaces to try and find a v2 interface.
    // The CMSIS-DAPv2 spec says that v2 interfaces should use a specific
    // WinUSB interface GUID, but in addition to being hard to read, the
    // official DAPLink firmware doesn't use it. Instead, we scan for an
    // interface whose string contains "CMSIS-DAP" and has two or three
    // endpoints of the correct type and direction.
    let c_desc = device.config_descriptor(0).ok()?;
    for interface in c_desc.interfaces() {
        for i_desc in interface.descriptors() {
            // Skip interfaces without "CMSIS-DAP" in their string
            match handle.read_interface_string(language, &i_desc, timeout) {
                Ok(i_str) if !i_str.contains("CMSIS-DAP") => continue,
                Err(_) => continue,
                Ok(_) => (),
            }

            // Skip interfaces without 2 or 3 endpoints
            let n_ep = i_desc.num_endpoints();
            if !(2..=3).contains(&n_ep) {
                continue;
            }

            let eps: Vec<_> = i_desc.endpoint_descriptors().collect();

            // Check the first interface is bulk out
            if eps[0].transfer_type() != rusb::TransferType::Bulk
                || eps[0].direction() != rusb::Direction::Out
            {
                continue;
            }

            // Check the second interface is bulk in
            if eps[1].transfer_type() != rusb::TransferType::Bulk
                || eps[1].direction() != rusb::Direction::In
            {
                continue;
            }

            // Detect a third bulk EP which will be for SWO streaming
            let mut swo_ep = None;

            if eps.len() > 2
                && eps[2].transfer_type() == rusb::TransferType::Bulk
                && eps[2].direction() == rusb::Direction::In
            {
                swo_ep = Some((eps[2].address(), eps[2].max_packet_size() as usize));
            }

            // Attempt to claim this interface
            match handle.claim_interface(interface.number()) {
                Ok(()) => {
                    log::debug!("Opening {:04x}:{:04x} in CMSIS-DAPv2 mode", vid, pid);
                    return Some(CmsisDapDevice::V2 {
                        handle,
                        out_ep: eps[0].address(),
                        in_ep: eps[1].address(),
                        swo_ep,
                        max_packet_size: eps[1].max_packet_size() as usize,
                    });
                }
                Err(_) => continue,
            }
        }
    }

    // Could not open in v2
    log::debug!(
        "Could not open {:04x}:{:04x} in CMSIS-DAP v2 mode",
        vid,
        pid
    );
    None
}

fn device_matches(
    device_descriptor: DeviceDescriptor,
    selector: &DebugProbeSelector,
    serial_str: Option<String>,
) -> bool {
    if device_descriptor.vendor_id() == selector.vendor_id
        && device_descriptor.product_id() == selector.product_id
    {
        if selector.serial_number.is_some() {
            serial_str == selector.serial_number
        } else {
            true
        }
    } else {
        false
    }
}

/// Attempt to open the given DebugProbeInfo in CMSIS-DAP v2 mode if possible,
/// otherwise in v1 mode.
pub fn open_device_from_selector(
    selector: impl Into<DebugProbeSelector>,
) -> Result<CmsisDapDevice, ProbeCreationError> {
    let selector = selector.into();

    log::trace!("Attempting to open device matching {}", selector);

    // We need to use rusb to detect the proper HID interface to use
    // if a probe has multiple HID interfaces. The hidapi lib unfortunately
    // offers no method to get the interface description string directly,
    // so we retrieve the device information using rusb and store it here.
    //
    // If rusb cannot be used, we will just use the first HID interface and
    // try to open that.
    let mut hid_device_info: Option<DebugProbeInfo> = None;

    // Try using rusb to open a v2 device. This might fail if
    // the device does not support v2 operation or due to driver
    // or permission issues with opening bulk devices.
    if let Ok(devices) = rusb::Context::new().and_then(|ctx| ctx.devices()) {
        for device in devices.iter() {
            log::trace!("Trying device {:?}", device);

            let d_desc = match device.device_descriptor() {
                Ok(d_desc) => d_desc,
                Err(err) => {
                    log::trace!("Error reading descriptor: {:?}", err);
                    continue;
                }
            };

            let handle = match device.open() {
                Ok(handle) => handle,
                Err(err) => {
                    log::trace!("Error opening: {:?}", err);
                    continue;
                }
            };

            let timeout = Duration::from_millis(100);
            let sn_str = match handle.read_languages(timeout) {
                Ok(langs) => langs.get(0).and_then(|lang| {
                    handle
                        .read_serial_number_string(*lang, &d_desc, timeout)
                        .ok()
                }),
                Err(err) => {
                    log::trace!("Error getting languages: {:?}", err);
                    continue;
                }
            };

            // We have to ensure the handle gets closed after reading the serial number,
            // multiple open handles are not allowed on Windows.
            drop(handle);

            if device_matches(d_desc, &selector, sn_str) {
                hid_device_info = get_cmsisdap_info(&device);

                if hid_device_info.is_some() {
                    // If the VID, PID, and potentially SN all match,
                    // and the device is a valid CMSIS-DAP probe,
                    // attempt to open the device in v2 mode.
                    if let Some(device) = open_v2_device(device) {
                        return Ok(device);
                    }
                }
            }
        }
    } else {
        log::debug!("No devices matched using rusb");
    }

    // If rusb failed or the device didn't support v2, try using hidapi to open in v1 mode.
    let vid = selector.vendor_id;
    let pid = selector.product_id;
    let sn = &selector.serial_number;

    log::debug!(
        "Attempting to open {:04x}:{:04x} in CMSIS-DAP v1 mode",
        vid,
        pid
    );

    // Attempt to open provided VID/PID/SN with hidapi

    let hid_api = HidApi::new()?;

    let mut device_list = hid_api.device_list();

    // We have to filter manually so that we can check the correct HID interface number.
    // Using HidApi::open() will return the first device which matches PID and VID,
    // which is not always what we want.
    let device_info = device_list
        .find(|info| {
            let mut device_match = info.vendor_id() == vid && info.product_id() == pid;

            if let Some(sn) = sn {
                device_match &= Some(sn.as_ref()) == info.serial_number();
            }

            if let Some(hid_interface) =
                hid_device_info.as_ref().and_then(|info| info.hid_interface)
            {
                device_match &= info.interface_number() == hid_interface as i32;
            }

            device_match
        })
        .ok_or(ProbeCreationError::NotFound)?;

    let device = device_info.open_device(&hid_api)?;

    match device.get_product_string() {
        Ok(Some(s)) if s.contains("CMSIS-DAP") => Ok(CmsisDapDevice::V1 {
            handle: device,
            // Start with a default 64-byte report size, which is the most
            // common size for CMSIS-DAPv1 HID devices. We'll request the
            // actual size to use from the probe later.
            report_size: 64,
        }),
        _ => {
            // Return NotFound if this VID:PID was not a valid CMSIS-DAP probe,
            // or if it couldn't be opened, so that other probe modules can
            // attempt to open it instead.
            Err(ProbeCreationError::NotFound)
        }
    }
}
