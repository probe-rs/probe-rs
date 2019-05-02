use crate::commands::general::info::FirmwareVersion;
use crate::commands::general::info::ProductID;
use crate::commands::general::info::VendorID;
use libusb::{Context, Device, DeviceHandle, Error};
use std::time::Duration;

use probe::debug_probe::DebugProbeError;

pub const TIMEOUT: Duration = Duration::from_millis(1000);

rental! {
    pub mod rent {
        use super::*;
        /// Provides low-level USB enumeration and transfers for STLinkV2/3 devices.
        #[rental]
        pub struct DAPLinkUSBDeviceRenter {
            context: Box<Context>,
            device: Box<Device<'context>>,
            device_handle: Box<DeviceHandle<'context>>,
        }
    }
}

// A helper struct to match STLink deviceinfo.
#[derive(Clone, Default)]
pub struct DAPLinkInfo {
    pub vendor_id: VendorID,
    pub product_id: ProductID,
    pub firmware_version: FirmwareVersion,
    ep_out: u8,
    ep_in: u8,
    ep_swv: u8,
}

pub use rent::DAPLinkUSBDeviceRenter;

pub struct DAPLinkUSBDevice {
    renter: DAPLinkUSBDeviceRenter,
    info: DAPLinkInfo,
}

fn usb_match<'a>(device: &Device<'a>) -> bool {
    if let Ok(descriptor) = device.device_descriptor() {
        // TODO: Poll DAPLinkInfo from device and check if the answer is valid.
        true
    } else {
        false
    }
}

pub fn get_all_plugged_devices<'a>(
    context: &'a Context,
) -> Result<Vec<(Device<'a>, DAPLinkInfo)>, DebugProbeError> {
    let devices = context.devices().map_err(|_| DebugProbeError::USBError)?;
    devices.iter()
            .filter(usb_match)
            .map(|d| {
                let descriptor = d.device_descriptor().map_err(|_| DebugProbeError::USBError)?;
                Ok((d, Default::default()))
            })
            .collect::<Result<Vec<_>, DebugProbeError>>()
}

impl DAPLinkUSBDevice {
    /// Creates and initializes a new USB device.
    pub fn new<F>(mut device_selector: F) -> Result<Self, DebugProbeError>
    where
        F: for<'a> FnMut(Vec<(Device<'a>, DAPLinkInfo)>) -> Result<Device<'a>, Error>
    {
        let context = Context::new().map_err(|_| DebugProbeError::USBError)?;

        let mut info = Default::default();

        let renter = DAPLinkUSBDeviceRenter::try_new(
            Box::new(context),
            |context| Ok(Box::new(device_selector(get_all_plugged_devices(context)?).map_err(|_| DebugProbeError::USBError)?)),
            |device, _context| {
                
                let mut device_handle = Box::new(device.open().map_err(|_| DebugProbeError::USBError)?);

                let config = device.active_config_descriptor().map_err(|_| DebugProbeError::USBError)?;
                let descriptor = device.device_descriptor().map_err(|_| DebugProbeError::USBError)?;

                device_handle.claim_interface(0).map_err(|_| DebugProbeError::USBError)?;

                if let Some(interface) = config.interfaces().next() {
                    if let Some(descriptor) = interface.descriptors().next() {
                        for endpoint in descriptor.endpoint_descriptors() {
                            // TODO: Check endpoint capability.
                        }
                    }
                }

                Ok(device_handle)
            },
        ).or_else(|_: rental::RentalError<_, std::boxed::Box<Context>>| Err(DebugProbeError::RentalInitError))?;

        let usb_daplink = Self {
            renter,
            info,
        };

        Ok(usb_daplink)
    }

    /// Writes and reads the given data to and from the correct endpoints.
    pub fn write(
        &mut self,
        request_data: &[u8],
        response_data: &mut [u8],
        timeout: Duration,
    ) -> Result<(), DebugProbeError> {
        let ep_out = self.info.ep_out;
        let ep_in = self.info.ep_in;

        // Send the given data to the USB endpoint.
        if !request_data.is_empty() {
            let written_bytes = self.renter.rent(|dh| dh.write_bulk(ep_out, request_data, timeout))
                                           .map_err(|_| DebugProbeError::USBError)?;
        }

        // Receive the expected answer from the USB endpoint.
        if !response_data.is_empty() {
            let read_bytes = self.renter.rent(|dh| dh.read_bulk(ep_in, response_data, timeout))
                                        .map_err(|_| DebugProbeError::USBError)?;
        }
        Ok(())
    }

    /// Closes the USB interface gracefully.
    /// Internal helper.
    fn close(&mut self) -> Result<(), Error> {
        self.renter.rent_mut(|dh| dh.release_interface(0))
    }
}

impl Drop for DAPLinkUSBDevice {
    /// This drop ensures we always release the USB device interface.
    fn drop(&mut self) {
        // We ignore the error case as we can't do much about it anyways.
        let _ = self.close();
    }
}
