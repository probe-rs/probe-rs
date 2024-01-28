mod d2xx;
pub mod error;
mod raw;

use std::io::{self, Read, Write};
use std::time::Duration;

use nusb::DeviceInfo;

use d2xx::FtdiD2xx;
use error::FtdiError;
use raw::FtdiRaw;

use crate::DebugProbeError;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChipType {
    Am,
    Bm,
    FT2232C,
    R,
    FT2232H,
    FT4232H,
    FT232H,
    FT230X,
}

#[repr(C)]
#[allow(unused)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BitMode {
    Reset = 0,
    Bitbang = 1,
    Mpsse = 2,
    SyncBb = 4,
    Mcu = 8,
    Opto = 16,
    Cbus = 32,
    SyncFf = 64,
    Ft1284 = 128,
}

#[repr(C)]
#[allow(unused)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Interface {
    A = 1,
    B = 2,
    C = 3,
    D = 4,
}

impl Interface {
    fn index(&self) -> u16 {
        *self as u16
    }
}

trait FtdiDriver: Send {
    fn usb_reset(&mut self) -> Result<()>;
    fn usb_purge_buffers(&mut self) -> Result<()>;
    fn set_usb_timeouts(&mut self, read_timeout: Duration, write_timeout: Duration) -> Result<()>;
    fn set_latency_timer(&mut self, value: u8) -> Result<()>;
    fn set_bitmode(&mut self, bitmask: u8, mode: BitMode) -> Result<()>;
    fn read_data(&mut self, data: &mut [u8]) -> io::Result<usize>;
    fn write_data(&mut self, data: &[u8]) -> io::Result<usize>;
}

pub struct Builder {
    interface: Interface,
    read_timeout: Duration,
    write_timeout: Duration,
}

impl Builder {
    pub const fn new() -> Self {
        Self {
            interface: Interface::A,
            read_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(5),
        }
    }

    pub const fn with_interface(mut self, interface: Interface) -> Self {
        self.interface = interface;
        self
    }

    pub const fn with_read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = timeout;
        self
    }

    pub const fn with_write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = timeout;
        self
    }

    pub fn usb_open(self, usb_device: DeviceInfo) -> Result<Device, DebugProbeError> {
        let mut device = Device::open(usb_device, self.interface)?;

        device
            .context
            .set_usb_timeouts(self.read_timeout, self.write_timeout)?;

        Ok(device)
    }
}

pub struct Device {
    context: Box<dyn FtdiDriver>,

    chip_type: Option<ChipType>,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // TODO print more information
        f.debug_struct("Device")
            .field("chip_type", &self.chip_type)
            .finish()
    }
}

impl Device {
    fn open(usb_device: DeviceInfo, interface: Interface) -> Result<Self, DebugProbeError> {
        // let context = Box::new(FtdiRaw::open(&usb_device, interface)?);
        let context: Box<dyn FtdiDriver> = if let Ok(d2xx) = FtdiD2xx::open(&usb_device, interface)
        {
            // note: fine with letting d2xx fail silently and Raw to provide the
            // traceback
            Box::new(d2xx)
        } else {
            Box::new(FtdiRaw::open(&usb_device, interface)?)
        };

        let chip_type = match (
            usb_device.device_version(),
            usb_device.serial_number().unwrap_or(""),
        ) {
            (0x400, _) | (0x200, "") => Some(ChipType::Bm),
            (0x200, _) => Some(ChipType::Am),
            (0x500, _) => Some(ChipType::FT2232C),
            (0x600, _) => Some(ChipType::R),
            (0x700, _) => Some(ChipType::FT2232H),
            (0x800, _) => Some(ChipType::FT4232H),
            (0x900, _) => Some(ChipType::FT232H),
            (0x1000, _) => Some(ChipType::FT230X),

            (version, _) => {
                tracing::warn!("Unknown FTDI device version: {:X?}", version);
                None
            }
        };

        tracing::debug!("Opened FTDI device: {:?}", chip_type);

        Ok(Self { context, chip_type })
    }

    pub fn usb_reset(&mut self) -> Result<()> {
        self.context.usb_reset()
    }

    pub fn usb_purge_buffers(&mut self) -> Result<()> {
        self.context.usb_purge_buffers()
    }

    pub fn set_latency_timer(&mut self, value: u8) -> Result<()> {
        self.context.set_latency_timer(value)
    }

    pub fn set_bitmode(&mut self, bitmask: u8, mode: BitMode) -> Result<()> {
        self.context.set_bitmode(bitmask, mode)
    }

    pub fn chip_type(&self) -> Option<ChipType> {
        self.chip_type
    }

    pub fn set_pins(&mut self, level: u16, direction: u16) -> Result<()> {
        self.context
            .write_data(&[0x80, level as u8, direction as u8])?;
        self.context
            .write_data(&[0x82, (level >> 8) as u8, (direction >> 8) as u8])?;

        Ok(())
    }

    pub fn disable_loopback(&mut self) -> Result<()> {
        self.context.write_data(&[0x85])?;
        Ok(())
    }

    pub fn disable_divide_by_5(&mut self) -> Result<()> {
        self.context.write_data(&[0x8A])?;
        Ok(())
    }

    pub fn enable_divide_by_5(&mut self) -> Result<()> {
        self.context.write_data(&[0x8B])?;
        Ok(())
    }

    pub fn configure_clock_divider(&mut self, divisor: u16) -> Result<()> {
        let [l, h] = divisor.to_le_bytes();
        self.context.write_data(&[0x86, l, h])?;
        Ok(())
    }
}

impl Read for Device {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.context.read_data(buf)
    }
}

impl Write for Device {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.context.write_data(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub type Result<T, E = FtdiError> = std::result::Result<T, E>;
