use crate::probe::cmsisdap::tools::open_v2_device;
use crate::probe::orbtrace::{OrbTraceDevice, OrbTraceSource, TraceInterface};
use crate::{DebugProbeInfo, DebugProbeSelector, ProbeCreationError};
use nusb::transfer::Direction;
use nusb::DeviceInfo;
use thiserror::Error;

const ORBTRACE_VID: u16 = 0x1209;
const ORBTRACE_PID: u16 = 0x3443;

fn is_orbtrace(device: &DeviceInfo) -> bool {
    device.vendor_id() == ORBTRACE_VID && device.product_id() == ORBTRACE_PID
}

/// Finds all ORBTrace devices.
///
/// This method uses nusb to read device strings, which might fail due
/// to permission or driver errors, so it falls back to listing only
/// HID devices if it does not find any suitable devices.
#[tracing::instrument(skip_all)]
pub fn list_orbtrace_devices() -> Vec<DebugProbeInfo> {
    tracing::debug!("Searching for ORBTrace probes using nusb");

    let probes = match nusb::list_devices() {
        Ok(devices) => devices
            .filter_map(|device| get_device_info(&device))
            .collect(),
        Err(e) => {
            tracing::warn!("error listing devices with nusb: {:?}", e);
            vec![]
        }
    };

    tracing::debug!("Found {} ORBTrace probes", probes.len());
    probes
}

/// Checks if a given Device is an ORBTrace probe, returning Some(DebugProbeInfo) if so.
fn get_device_info(device: &DeviceInfo) -> Option<DebugProbeInfo> {
    // Check VID and PID to see if this is an ORBTrace probe.
    if !is_orbtrace(device) {
        return None;
    }

    let prod_str = device.product_string().unwrap_or("");
    let sn_str = device.serial_number();

    Some(DebugProbeInfo::new(
        prod_str.to_string(),
        device.vendor_id(),
        device.product_id(),
        sn_str.map(Into::into),
        &OrbTraceSource,
        None,
    ))
}

/// Attempt to open the given DebugProbeInfo in CMSIS-DAP v2 mode.
pub fn open_device_from_selector(
    selector: &DebugProbeSelector,
) -> Result<OrbTraceDevice, ProbeCreationError> {
    tracing::trace!("Attempting to open device matching {}", selector);

    // Try using nusb to open a v2 device. This might fail if
    // the device does not support v2 operation or due to driver
    // or permission issues with opening bulk devices.
    if let Ok(devices) = nusb::list_devices() {
        for device in devices {
            tracing::trace!("Trying device {:?}", device);

            if !is_orbtrace(&device) {
                continue;
            }

            if selector.matches(&device) {
                // If the VID, PID, and potentially SN all match,
                // and the device is a valid CMSIS-DAP probe,
                // attempt to open the device in v2 mode.
                match open_device(&device) {
                    Ok(device) => return Ok(device),
                    Err(e) => {
                        tracing::warn!("Error opening device: {e:?}");
                    }
                }
            }
        }
    }

    tracing::debug!("No devices matched using nusb");
    Err(ProbeCreationError::NotFound)
}

/// Attempt to open the given device in CMSIS-DAP v2 mode
fn open_device(device_info: &DeviceInfo) -> Result<OrbTraceDevice, OpenError> {
    let cmsis_dap = open_v2_device(device_info).ok_or(OpenError::Dap)?;
    let trace = open_trace_interface(device_info)?;

    Ok(OrbTraceDevice { cmsis_dap, trace })
}

fn open_trace_interface(device: &DeviceInfo) -> Result<TraceInterface, OpenError> {
    let device = device.open().map_err(OpenError::Device)?;
    let configuration = device
        .active_configuration()
        .map_err(OpenError::ActiveConfiguration)?;

    // Find and open trace interface.
    for interface in configuration.interfaces() {
        let Some(alt_setting) = interface.alt_settings().next() else {
            continue;
        };
        if alt_setting.class() == 0xff && alt_setting.subclass() == b'T' {
            if let Some(ep) = alt_setting.endpoints().next() {
                if ep.direction() == Direction::In {
                    let handle = device
                        .claim_interface(interface.interface_number())
                        .map_err(OpenError::ClaimInterface)?;
                    return Ok(TraceInterface {
                        handle,
                        interface_number: interface.interface_number(),
                        endpoint: ep.address(),
                        max_packet_size: ep.max_packet_size(),
                    });
                }
            }
        }
    }
    Err(OpenError::NoTraceInterface)
}

#[derive(Debug, Error)]
enum OpenError {
    #[error("Error opening DAP device")]
    Dap,
    #[error("Error opening USB device")]
    Device(#[source] nusb::Error),
    #[error("Error getting active configuration")]
    ActiveConfiguration(#[source] nusb::descriptors::ActiveConfigurationError),
    #[error("Cannot find trace interface")]
    NoTraceInterface,
    #[error("Error claiming interface")]
    ClaimInterface(#[source] nusb::Error),
}
