use super::CmsisDapDevice;
use crate::probe::{
    cmsisdap::CmsisDapFactory, DebugProbeInfo, DebugProbeSelector, ProbeCreationError,
};
use hidapi::HidApi;
use nusb::{
    transfer::{Direction, EndpointType},
    DeviceInfo,
};

const USB_CLASS_HID: u8 = 0x03;

/// Finds all CMSIS-DAP devices, either v1 (HID) or v2 (WinUSB Bulk).
///
/// This method uses nusb to read device strings, which might fail due
/// to permission or driver errors, so it falls back to listing only
/// HID devices if it does not find any suitable devices.
#[tracing::instrument(skip_all)]
pub fn list_cmsisdap_devices() -> Vec<DebugProbeInfo> {
    tracing::debug!("Searching for CMSIS-DAP probes using nusb");

    let mut probes = match nusb::list_devices() {
        Ok(devices) => devices
            .filter_map(|device| get_cmsisdap_info(&device))
            .collect(),
        Err(e) => {
            tracing::warn!("error listing devices with nusb: {:?}", e);
            vec![]
        }
    };

    tracing::debug!(
        "Found {} CMSIS-DAP probes using nusb, searching HID",
        probes.len()
    );

    if let Ok(api) = hidapi::HidApi::new() {
        for device in api.device_list() {
            if let Some(info) = get_cmsisdap_hid_info(device) {
                if !probes.iter().any(|p| {
                    p.vendor_id == info.vendor_id
                        && p.product_id == info.product_id
                        && p.serial_number == info.serial_number
                }) {
                    tracing::trace!("Adding new HID-only probe {:?}", info);
                    probes.push(info)
                } else {
                    tracing::trace!("Ignoring duplicate {:?}", info);
                }
            }
        }
    }

    tracing::debug!("Found {} CMSIS-DAP probes total", probes.len());
    probes
}

/// Checks if a given Device is a CMSIS-DAP probe, returning Some(DebugProbeInfo) if so.
fn get_cmsisdap_info(device: &DeviceInfo) -> Option<DebugProbeInfo> {
    // Open device handle and read basic information
    let prod_str = device.product_string().unwrap_or("");
    let sn_str = device.serial_number();

    // Most CMSIS-DAP probes say something like "CMSIS-DAP"
    let cmsis_dap_product = is_cmsis_dap(prod_str) || is_known_cmsis_dap_dev(device);

    // Iterate all interfaces, looking for:
    // 1. Any with CMSIS-DAP in their interface string
    // 2. Any that are HID, if the product string says CMSIS-DAP,
    //    to save for potential HID-only operation.
    let mut cmsis_dap_interface = false;
    let mut hid_interface = None;
    for interface in device.interfaces() {
        let Some(interface_desc) = interface.interface_string() else {
            tracing::trace!(
                "interface {} has no string, skipping",
                interface.interface_number()
            );
            continue;
        };
        if is_cmsis_dap(interface_desc) {
            tracing::trace!(
                "  Interface {}: {}",
                interface.interface_number(),
                interface_desc
            );
            cmsis_dap_interface = true;
            if interface.class() == USB_CLASS_HID {
                tracing::trace!("    HID interface found");
                hid_interface = Some(interface.interface_number());
            }
        }
    }

    if cmsis_dap_interface || cmsis_dap_product {
        tracing::trace!(
            "{}: CMSIS-DAP device with {} interfaces",
            prod_str,
            device.interfaces().count()
        );

        if let Some(interface) = hid_interface {
            tracing::trace!("Will use interface number {} for CMSIS-DAPv1", interface);
        } else {
            tracing::trace!("No HID interface for CMSIS-DAP found.")
        }

        Some(DebugProbeInfo::new(
            prod_str.to_string(),
            device.vendor_id(),
            device.product_id(),
            sn_str.map(Into::into),
            &CmsisDapFactory,
            hid_interface,
        ))
    } else {
        None
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
        ))
    } else {
        None
    }
}

/// Attempt to open the given device in CMSIS-DAP v2 mode
pub fn open_v2_device(device_info: &DeviceInfo) -> Option<CmsisDapDevice> {
    // Open device handle and read basic information
    let vid = device_info.vendor_id();
    let pid = device_info.product_id();

    let device = device_info.open().ok()?;

    // Go through interfaces to try and find a v2 interface.
    // The CMSIS-DAPv2 spec says that v2 interfaces should use a specific
    // WinUSB interface GUID, but in addition to being hard to read, the
    // official DAPLink firmware doesn't use it. Instead, we scan for an
    // interface whose string like "CMSIS-DAP" and has two or three
    // endpoints of the correct type and direction.
    let c_desc = device.configurations().next()?;
    for interface in c_desc.interfaces() {
        for i_desc in interface.alt_settings() {
            // Skip interfaces without "CMSIS-DAP" like pattern in their string
            let Some(interface_str) = device_info
                .interfaces()
                .find(|i| i.interface_number() == interface.interface_number())
                .and_then(|i| i.interface_string())
            else {
                continue;
            };
            if !is_cmsis_dap(interface_str) {
                continue;
            }

            // Skip interfaces without 2 or 3 endpoints
            let n_ep = i_desc.num_endpoints();
            if !(2..=3).contains(&n_ep) {
                continue;
            }

            let eps: Vec<_> = i_desc.endpoints().collect();

            // Check the first endpoint is bulk out
            if eps[0].transfer_type() != EndpointType::Bulk || eps[0].direction() != Direction::Out
            {
                continue;
            }

            // Check the second endpoint is bulk in
            if eps[1].transfer_type() != EndpointType::Bulk || eps[1].direction() != Direction::In {
                continue;
            }

            // Detect a third bulk EP which will be for SWO streaming
            let mut swo_ep = None;

            if eps.len() > 2
                && eps[2].transfer_type() == EndpointType::Bulk
                && eps[2].direction() == Direction::In
            {
                swo_ep = Some((eps[2].address(), eps[2].max_packet_size()));
            }

            // Attempt to claim this interface
            match device.claim_interface(interface.interface_number()) {
                Ok(handle) => {
                    tracing::debug!("Opening {:04x}:{:04x} in CMSIS-DAPv2 mode", vid, pid);
                    return Some(CmsisDapDevice::V2 {
                        handle,
                        out_ep: eps[0].address(),
                        in_ep: eps[1].address(),
                        swo_ep,
                        max_packet_size: eps[1].max_packet_size(),
                    });
                }
                Err(_) => continue,
            }
        }
    }

    // Could not open in v2
    tracing::debug!(
        "Could not open {:04x}:{:04x} in CMSIS-DAP v2 mode",
        vid,
        pid
    );
    None
}

/// Attempt to open the given DebugProbeInfo in CMSIS-DAP v2 mode if possible,
/// otherwise in v1 mode.
pub fn open_device_from_selector(
    selector: &DebugProbeSelector,
) -> Result<CmsisDapDevice, ProbeCreationError> {
    tracing::trace!("Attempting to open device matching {}", selector);

    // We need to use nusb to detect the proper HID interface to use
    // if a probe has multiple HID interfaces. The hidapi lib unfortunately
    // offers no method to get the interface description string directly,
    // so we retrieve the device information using nusb and store it here.
    //
    // If nusb cannot be used, we will just use the first HID interface and
    // try to open that.
    let mut hid_device_info = None;

    // Try using nusb to open a v2 device. This might fail if
    // the device does not support v2 operation or due to driver
    // or permission issues with opening bulk devices.
    if let Ok(devices) = nusb::list_devices() {
        for device in devices {
            tracing::trace!("Trying device {:?}", device);

            if selector.matches(&device) {
                hid_device_info = get_cmsisdap_info(&device);

                if hid_device_info.is_some() {
                    // If the VID, PID, and potentially SN all match,
                    // and the device is a valid CMSIS-DAP probe,
                    // attempt to open the device in v2 mode.
                    if let Some(device) = open_v2_device(&device) {
                        return Ok(device);
                    }
                }
            }
        }
    } else {
        tracing::debug!("No devices matched using nusb");
    }

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

    let hid_api = HidApi::new()?;

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
        Ok(Some(s)) if is_cmsis_dap(&s) => {
            Ok(CmsisDapDevice::V1 {
                handle: device,
                // Start with a default 64-byte report size, which is the most
                // common size for CMSIS-DAPv1 HID devices. We'll request the
                // actual size to use from the probe later.
                report_size: 64,
            })
        }
        _ => {
            // Return NotFound if this VID:PID was not a valid CMSIS-DAP probe,
            // or if it couldn't be opened, so that other probe modules can
            // attempt to open it instead.
            Err(ProbeCreationError::NotFound)
        }
    }
}

/// We recognise cmsis dap interfaces if they have string like "CMSIS-DAP"
/// in them. As devices spell CMSIS DAP differently we go through known
/// spellings/patterns looking for a match
fn is_cmsis_dap(id: &str) -> bool {
    id.contains("CMSIS-DAP") || id.contains("CMSIS_DAP")
}

/// Some devices don't have a CMSIS-DAP interface string, but are still
/// CMSIS-DAP probes. We hardcode a list of known VID/PID pairs here.
fn is_known_cmsis_dap_dev(device: &DeviceInfo) -> bool {
    // - 1a86:8012 WCH-Link in DAP mode, This shares the same description string as the
    //   WCH-Link in RV mode, so we have to check by vendor ID and product ID.
    const KNOWN_DAPS: &[(u16, u16)] = &[(0x1a86, 0x8012)];

    KNOWN_DAPS
        .iter()
        .any(|&(vid, pid)| device.vendor_id() == vid && device.product_id() == pid)
}
