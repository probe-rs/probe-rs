use std::time::Duration;

use libftd2xx::{Ftdi, FtdiCommon};
use nusb::DeviceInfo;

use crate::DebugProbeError;

use super::{error::FtdiError, FtdiDriver, Interface, Result};
// todo https://docs.rs/libloading/latest/libloading/

/// An FTDI driver using the proprietary D2XX driver installed by default in
/// Windows. Should technically work in Linux, but untested
pub struct FtdiD2xx {
    ft: Ftdi,
}

fn ft_status_to_lib_err(e: libftd2xx::FtStatus) -> FtdiError {
    FtdiError::Other(format!("FTDI D2XX error: {e}"))
}

fn ft_status_to_io_err(e: libftd2xx::FtStatus) -> std::io::Error {
    std::io::Error::other(format!("FTDI D2XX error {e}"))
}

impl FtdiDriver for FtdiD2xx {
    fn usb_reset(&mut self) -> Result<()> {
        self.ft.reset().map_err(ft_status_to_lib_err)
    }

    fn usb_purge_buffers(&mut self) -> Result<()> {
        self.ft.purge_all().map_err(ft_status_to_lib_err)
    }

    fn set_usb_timeouts(
        &mut self,
        read_timeout: std::time::Duration,
        write_timeout: std::time::Duration,
    ) -> Result<()> {
        self.ft
            .set_timeouts(read_timeout, write_timeout)
            .map_err(ft_status_to_lib_err)
    }

    fn set_latency_timer(&mut self, value: u8) -> Result<()> {
        self.ft
            .set_latency_timer(Duration::from_millis(value as u64))
            .map_err(ft_status_to_lib_err)
    }

    fn set_bitmode(&mut self, bitmask: u8, mode: super::BitMode) -> Result<()> {
        self.ft
            .set_bit_mode(bitmask, (mode as u8).into())
            .map_err(ft_status_to_lib_err)
    }

    fn read_data(&mut self, data: &mut [u8]) -> std::io::Result<usize> {
        self.ft.read(data).map_err(ft_status_to_io_err)
    }

    fn write_data(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.ft.write(data).map_err(ft_status_to_io_err)
    }
}

impl FtdiD2xx {
    pub fn open(usb_device: &DeviceInfo, interface: Interface) -> Result<Self, DebugProbeError> {
        if interface != Interface::A {
            return Err(DebugProbeError::NotImplemented(
                "Non-default FTDI interfaces",
            ));
        }
        let Some(serial) = usb_device.serial_number() else {
            // todo: this is probably overly strict
            return Err(DebugProbeError::Usb(std::io::Error::other(format!(
                "cannot open FTDI D2XX device with no serial",
            ))));
        };

        let ft = Ftdi::with_serial_number(serial).map_err(|e| {
            DebugProbeError::Usb(std::io::Error::other(format!(
                "error opening FTDI D2XX device: {e}",
            )))
        })?;

        Ok(Self { ft })
    }
}
