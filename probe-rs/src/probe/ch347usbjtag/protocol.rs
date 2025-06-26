use std::time::Duration;

use bitvec::vec::BitVec;
use nusb::{DeviceInfo, Interface};

use crate::probe::{
    self, DebugProbeError, DebugProbeInfo, DebugProbeKind, DebugProbeSelector, ProbeCreationError,
    UsbFilters, usb_util::InterfaceExt,
};

use super::Ch347UsbJtagFactory;

const CH34X_VID_PID: [(u16, u16); 3] = [(0x1A86, 0x55DE), (0x1A86, 0x55DD), (0x1A86, 0x55E8)];

pub(crate) fn is_ch34x_device(device: &DeviceInfo) -> bool {
    CH34X_VID_PID.contains(&(device.vendor_id(), device.product_id()))
}

#[derive(Debug, Clone, Copy)]
enum Pack {
    StandardPack,
    LargePack,
}

#[derive(Debug, Clone, Copy)]
enum Command {
    Clock { tms: bool, tdi: bool, capture: bool },
}

impl From<Command> for u8 {
    fn from(value: Command) -> Self {
        match value {
            Command::Clock { tms, tdi, .. } => (u8::from(tms) << 1) | (u8::from(tdi) << 4),
        }
    }
}

struct Clock {
    tms: bool,
    tdi: bool,
    trst: bool,
}

impl From<Clock> for u8 {
    fn from(value: Clock) -> Self {
        let Clock { tms, tdi, trst } = value;
        u8::from(tms) << 1 | u8::from(tdi) << 4 | u8::from(trst) << 5
    }
}

/// Ch347 device, whitch is a usb to gpio/i2c/spi/jtag/swd
/// ch347 has different packages, ch347f and ch347t
/// ch347t work mode depend on pin state on bool
/// ch347f full work
pub struct Ch347UsbJtagDevice {
    device: Interface,
    name: String,
    comand_quene: Vec<Command>,
    response: BitVec,
    /// default 0x06
    epout: u8,
    /// default 0x86
    epin: u8,
    pack: Pack,
    speed_khz: u32,
}

impl std::fmt::Debug for Ch347UsbJtagDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ch347UsbJtagDevice")
            .field("name", &self.name)
            .field("epout", &self.epout)
            .field("epin", &self.epin)
            .field("pack", &self.pack)
            .field("speed", &self.speed_khz)
            .finish()
    }
}

impl Ch347UsbJtagDevice {
    pub(crate) fn new_from_selector(
        selector: &DebugProbeSelector,
    ) -> Result<Self, ProbeCreationError> {
        let device = nusb::list_devices()
            .map_err(ProbeCreationError::Usb)?
            .filter(is_ch34x_device)
            .find(|device| selector.matches(device))
            .ok_or(ProbeCreationError::NotFound)?;

        let device_handle = device.open().map_err(probe::ProbeCreationError::Usb)?;

        let config = device_handle
            .configurations()
            .next()
            .expect("Can get usb device configs");

        tracing::info!("Active config descriptor: {:?}", config);

        // TODO, ch347t is different for ch347f
        // for interface in config.interfaces() {
        //     let interface_number = interface.interface_number();
        //
        //     let Some(descriptor) = interface.alt_settings().next() else {
        //         continue;
        //     };
        //
        //     if (!(descriptor.class() != 255
        //         && descriptor.subclass() != 0
        //         && descriptor.protocol() != 0))
        //     {
        //         continue;
        //     }
        // }

        // ch347f default in 4
        // buf ch347t i dnot know as i not have
        let interface = device_handle
            .claim_interface(4)
            .map_err(ProbeCreationError::Usb)?;

        // set 15MHz speed, and check pack mode
        let mut obuf = [0xD0, 0x06, 0x00, 0x00, 0x07, 0x30, 0x30, 0x30, 0x30];
        let mut ibuf = [0; 4];
        let pack;
        interface
            .write_bulk(0x06, &obuf, Duration::from_millis(500))
            .map_err(ProbeCreationError::Usb)?;

        interface
            .read_bulk(0x86, &mut ibuf, Duration::from_millis(500))
            .map_err(ProbeCreationError::Usb)?;

        // check the pack mode
        if ibuf[0] == 0xD0 && ibuf[3] == 0x00 {
            // LARGER_Pack Mode
            obuf[4] = 5;
            pack = Pack::LargePack;
        } else {
            obuf[4] = 3;
            pack = Pack::StandardPack;
        }

        // set default 15MHz
        interface
            .write_bulk(0x06, &obuf, Duration::from_millis(500))
            .map_err(ProbeCreationError::Usb)?;

        interface
            .read_bulk(0x86, &mut ibuf, Duration::from_millis(500))
            .map_err(ProbeCreationError::Usb)?;

        Ok(Self {
            device: interface,
            name: "ch347".into(),
            comand_quene: Vec::new(),
            response: BitVec::new(),
            epout: 0x06,
            epin: 0x86,
            pack,
            speed_khz: 15000,
        })
    }

    pub(crate) fn attach(&mut self) -> Result<(), DebugProbeError> {
        self.apply_clock_speed(self.speed_khz)?;
        Ok(())
    }

    pub(crate) fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    pub(crate) fn set_speed_khz(&mut self, speed_khz: u32) -> u32 {
        self.speed_khz = speed_khz;
        self.speed_khz
    }

    fn pack(&self) -> Pack {
        self.pack
    }

    // with speed index: 468.75Khz, 937.5KHz, 1.875MHz, 3.75MHz, 7.5MHz, 15MHz, 30MHz, 60Mhz
    // STANDARD_Pack start from 1.875MHz
    // LARGER_Pack start from 468.75KHz
    fn apply_clock_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        let mut buf = [0; 4];
        let index = match self.pack() {
            Pack::StandardPack => match speed_khz {
                1875 => 0,
                3750 => 1,
                7500 => 2,
                15000 => 3,
                30000 => 4,
                60000 => 5,
                _ => return Err(DebugProbeError::UnsupportedSpeed(speed_khz)),
            },
            Pack::LargePack => match speed_khz {
                468 => 0,
                937 => 1,
                1875 => 2,
                3750 => 3,
                7500 => 4,
                15000 => 5,
                30000 => 6,
                60000 => 7,
                _ => return Err(DebugProbeError::UnsupportedSpeed(speed_khz)),
            },
        };
        self.device
            .write_bulk(
                self.epout,
                &[0xD0, 0x06, 0x00, 0x00, index, 0x00, 0x00, 0x00, 0x00],
                Duration::from_millis(500),
            )
            .map_err(DebugProbeError::Usb)?;

        self.device
            .read_bulk(self.epin, &mut buf, Duration::from_millis(500))
            .map_err(DebugProbeError::Usb)?;
        if buf[3] == 0x00 {
            Ok(speed_khz)
        } else {
            Err(DebugProbeError::UnsupportedSpeed(speed_khz))
        }
    }

    fn flush(&mut self) -> Result<(), DebugProbeError> {
        let mut buffer = [0; 130];
        let mut obuf = vec![];
        let mut command = vec![0xD2];

        for &i in self.comand_quene.iter() {
            let byte = u8::from(i);
            // the byte is clock low, bit 0 = 1 that clock high
            obuf.push(byte);
            obuf.push(byte | 0x01);
        }
        command.extend_from_slice(&(obuf.len() as u16).to_le_bytes());
        command.extend_from_slice(&obuf);

        self.device
            .write_bulk(self.epout, &command, Duration::from_millis(500))
            .map_err(ProbeCreationError::Usb)?;
        self.device
            .read_bulk(self.epin, &mut buffer, Duration::from_millis(100))
            .map_err(ProbeCreationError::Usb)?;

        for (&c, &byte) in self.comand_quene.iter().zip(&buffer[3..]) {
            let Command::Clock { capture, .. } = c;
            if capture {
                self.response.push(byte != 0x00);
            }
        }

        self.comand_quene.clear();
        Ok(())
    }

    pub(crate) fn shift_bit(
        &mut self,
        tms: bool,
        tdi: bool,
        capture: bool,
    ) -> Result<(), DebugProbeError> {
        // max clock len is 127
        if self.comand_quene.len() >= 127 {
            self.flush()?;
        }
        self.comand_quene.push(Command::Clock { tms, tdi, capture });
        Ok(())
    }

    pub(crate) fn read_captured_bits(&mut self) -> Result<BitVec, DebugProbeError> {
        self.flush()?;
        Ok(std::mem::take(&mut self.response))
    }
}

pub(super) fn list_ch347usbjtag_devices() -> Vec<DebugProbeInfo> {
    let Ok(devices) = nusb::list_devices() else {
        return vec![];
    };

    devices
        .filter(is_ch34x_device)
        .map(|device| {
            DebugProbeInfo::new(
                "CH347 USB Jtag".to_string(),
                DebugProbeKind::Usb {
                    vendor_id: device.vendor_id(),
                    product_id: device.product_id(),
                    filters: UsbFilters {
                        serial_number: device.serial_number().map(Into::into),
                        hid_interface: None,

                        #[cfg(any(target_os = "linux", target_os = "android"))]
                        sysfs_path: Some(device.sysfs_path().to_owned()),

                        #[cfg(target_os = "windows")]
                        instance_id: Some(device.instance_id().display().to_string()),
                        #[cfg(target_os = "windows")]
                        parent_instance_id: Some(device.parent_instance_id().display().to_string()),
                        #[cfg(target_os = "windows")]
                        port_number: Some(device.port_number()),
                        #[cfg(target_os = "windows")]
                        driver: device.driver().map(str::to_string),

                        #[cfg(target_os = "macos")]
                        registry_id: Some(device.registry_entry_id()),
                        #[cfg(target_os = "macos")]
                        location_id: Some(device.location_id()),
                    },
                },
                &Ch347UsbJtagFactory,
            )
        })
        .collect()
}
