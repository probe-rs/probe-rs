//! CMSIS-DAP v1 probes, which use HID.

use super::{is_cmsis_dap, reject_probe_by_version};
use crate::probe::{
    DebugProbeInfo, ProbeCreationError, ProbeTransport, UsbSelector,
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
        let ProbeTransport::Usb(hid) = &info.transport;
        let duplicate = probes.iter().any(|p| {
            let ProbeTransport::Usb(usb) = &p.info.transport;
            usb.vendor_id == hid.vendor_id
                && usb.product_id == hid.product_id
                && usb.serial_number.as_deref().unwrap_or("")
                    == hid.serial_number.as_deref().unwrap_or("")
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
    selector: &UsbSelector,
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

            if let Some(hid_interface) = hid_device_info.as_ref().and_then(|info| {
                let ProbeTransport::Usb(info) = &info.transport;
                info.interface.filter(|_| info.is_hid_interface)
            }) {
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
            let report_size = descriptor_report_size(&device)
                .map_or_else(|| hid_report_size(device_info), usize::from);
            Ok((
                CmsisDapDevice::V1 {
                    handle: device,
                    report_size,
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

/// The size of the output report in the HID interface's report descriptor.
///
/// Every write has to be exactly this size. On Linux a shorter one reaches the probe as it is,
/// and a probe whose reports span several USB packets gathers several writes into one report.
///
/// Returns `None` if the descriptor cannot be read, or [`output_report_size`] finds no size in it.
fn descriptor_report_size(device: &hidapi::HidDevice) -> Option<u16> {
    let mut descriptor = [0; hidapi::MAX_REPORT_DESCRIPTOR_SIZE];
    let len = device.get_report_descriptor(&mut descriptor).ok()?;
    let size = output_report_size(&descriptor[..len]);
    tracing::debug!("HID report size from the report descriptor: {size:?}");
    size
}

/// The size in bytes of the output report a HID report descriptor describes: the Report Size
/// times the Report Count of every Output item, added up.
///
/// Returns `None` for a descriptor with report IDs or long items, which CMSIS-DAP does not use,
/// and for one that is malformed or gives a size no DAP packet can have.
fn output_report_size(descriptor: &[u8]) -> Option<u16> {
    // Item tags and types: the prefix without its size bits (HID 1.11, 6.2.2).
    const OUTPUT: u8 = 0x90;
    const REPORT_SIZE: u8 = 0x74;
    const REPORT_ID: u8 = 0x84;
    const REPORT_COUNT: u8 = 0x94;
    const PUSH: u8 = 0xA4;
    const POP: u8 = 0xB4;
    const LONG_ITEM: u8 = 0xFE;

    let (mut report_size, mut report_count) = (0u32, 0u32);
    let mut pushed = Vec::new();
    let mut bits = 0u32;
    let mut items = descriptor;
    while let Some((&prefix, rest)) = items.split_first() {
        if prefix == LONG_ITEM {
            return None;
        }
        let (data, rest) = rest.split_at_checked([0, 1, 2, 4][usize::from(prefix & 0x03)])?;
        let mut bytes = [0; 4];
        bytes[..data.len()].copy_from_slice(data);
        let value = u32::from_le_bytes(bytes);
        match prefix & !0x03 {
            REPORT_SIZE => report_size = value,
            REPORT_COUNT => report_count = value,
            REPORT_ID => return None,
            PUSH => pushed.push((report_size, report_count)),
            POP => (report_size, report_count) = pushed.pop()?,
            OUTPUT => bits = bits.checked_add(report_size.checked_mul(report_count)?)?,
            _ => {}
        }
        items = rest;
    }

    u16::try_from(bits.div_ceil(8))
        .ok()
        .filter(|&size| size > 0)
}

/// Manual override of HID report size, for when the report descriptor gives none.
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

#[cfg(test)]
mod tests {
    use super::output_report_size;

    /// A vendor-defined descriptor in the shape of TinyUSB's `TUD_HID_REPORT_DESC_GENERIC_INOUT`,
    /// with `items` inside its application collection.
    fn generic(items: &[&[u8]]) -> Vec<u8> {
        let mut descriptor = vec![0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01];
        descriptor.extend(items.concat());
        descriptor.push(0xc0);
        descriptor
    }

    const IN_64: &[u8] = &[
        0x09, 0x02, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40, 0x81, 0x02,
    ];
    const OUT_64: &[u8] = &[
        0x09, 0x03, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40, 0x91, 0x02,
    ];
    /// The Report Count of 512 takes a two-byte item.
    const OUT_512: &[u8] = &[
        0x09, 0x03, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x96, 0x00, 0x02, 0x91, 0x02,
    ];
    const REPORT_ID_1: &[u8] = &[0x85, 0x01];
    /// Output after a Pop uses the Report Count from before the Push.
    const PUSH_POP: &[u8] = &[
        0x75, 0x08, 0x95, 0x40, 0xa4, 0x96, 0x00, 0x02, 0x09, 0x02, 0x81, 0x02, 0xb4, 0x09, 0x03,
        0x91, 0x02,
    ];
    const LONG_ITEM: &[u8] = &[0xfe, 0x01, 0x10, 0x00];
    /// One byte more than a DAP packet can hold.
    const OUT_HUGE: &[u8] = &[0x75, 0x08, 0x97, 0x00, 0x00, 0x01, 0x00, 0x91, 0x02];
    /// An Output item of nearly 2^64 bits.
    const OUT_MUL_OVERFLOW: &[u8] = &[
        0x77, 0xff, 0xff, 0xff, 0xff, 0x97, 0xff, 0xff, 0xff, 0xff, 0x91, 0x02,
    ];
    /// Two Output items of nearly 2^32 bits each.
    const OUT_ADD_OVERFLOW: &[u8] = &[0x76, 0xff, 0xff, 0x96, 0xff, 0xff, 0x91, 0x02, 0x91, 0x02];

    #[test]
    fn output_report_size_of_descriptors() {
        // Ends inside the Output item, which is missing its data byte.
        let truncated = generic(&[IN_64, OUT_64]);
        let truncated = truncated[..truncated.len() - 2].to_vec();
        let cases: &[(&str, Vec<u8>, Option<u16>)] = &[
            ("64-byte reports", generic(&[IN_64, OUT_64]), Some(64)),
            ("bigger output", generic(&[IN_64, OUT_512]), Some(512)),
            ("report ID", generic(&[REPORT_ID_1, IN_64, OUT_64]), None),
            ("no output report", generic(&[IN_64]), None),
            ("truncated item", truncated, None),
            ("push and pop", generic(&[PUSH_POP]), Some(64)),
            ("long item", generic(&[LONG_ITEM, IN_64, OUT_64]), None),
            ("too big for a packet", generic(&[OUT_HUGE]), None),
            ("product overflow", generic(&[OUT_MUL_OVERFLOW]), None),
            ("sum overflow", generic(&[OUT_ADD_OVERFLOW]), None),
        ];
        for (name, descriptor, expected) in cases {
            assert_eq!(output_report_size(descriptor), *expected, "{name}");
        }
    }
}
