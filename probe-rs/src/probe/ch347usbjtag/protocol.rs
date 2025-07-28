use std::time::Duration;

use bitvec::vec::BitVec;
use nusb::{
    DeviceInfo, Interface,
    transfer::{Direction, EndpointType},
};

use crate::{
    architecture::arm::{ArmError, RegisterAddress},
    probe::{
        self, DebugProbeError, DebugProbeInfo, DebugProbeSelector, ProbeCreationError,
        WireProtocol, usb_util::InterfaceExt,
    },
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
struct JtagClock {
    tms: bool,
    tdi: bool,
    capture: bool,
}

impl From<JtagClock> for u8 {
    fn from(value: JtagClock) -> Self {
        let JtagClock { tms, tdi, .. } = value;
        (u8::from(tms) << 1) | (u8::from(tdi) << 4)
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
    epout: u8,
    epin: u8,
    pack: Pack,
    speed_khz: u32,
    protocol: Option<WireProtocol>,
    /// for jtag
    jtag_clocks: Vec<JtagClock>,
    jtag_bits: BitVec,
}

impl std::fmt::Debug for Ch347UsbJtagDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ch347UsbJtagDevice").finish()
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

        let mut found = None;

        for interface in config.interfaces() {
            let interface_num = interface.interface_number();

            tracing::info!("interface num: {}", interface_num);

            let Some(desc) = interface.alt_settings().next() else {
                continue;
            };

            if !(desc.class() == 0xff && desc.subclass() == 0x00 && desc.protocol() == 0x00) {
                tracing::info!("skip {interface_num} with wrong class/subclass/protocol");
                continue;
            }

            let mut epin = None;
            let mut epout = None;
            for endpoint in desc.endpoints() {
                let address = endpoint.address();
                tracing::info!("Endpoint {address:#04x}");
                if endpoint.transfer_type() != EndpointType::Bulk {
                    tracing::info!("skip endpoint {address:#04x}");
                    continue;
                }
                if endpoint.direction() == Direction::In {
                    epin = Some(address)
                } else {
                    epout = Some(address)
                }
            }

            // have been found interface
            if let (Some(epin), Some(epout)) = (epin, epout) {
                found = Some((interface_num, epin, epout));
                break;
            }
        }

        let Some((interface_num, epin, epout)) = found else {
            panic!("Not found ch347 interface, is that you in current mode");
        };

        tracing::info!(
            "Found ch347 current interface: \r\n\tinterface_num: {interface_num},\r\n\tepin: {epin:#04x}\r\n\tepout: {epout:#04x}"
        );

        let interface = device_handle.claim_interface(interface_num).unwrap();

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
            obuf[4] = 2;
            pack = Pack::StandardPack;
        }

        // set default 15MHz as SWD protocol
        interface
            .write_bulk(
                0x06,
                &[
                    0xe5, 0x08, 0x00, 0x40, 0x42, 0x0f, 0x00, 0x03, 0x00, 0x00, 0x00,
                ],
                Duration::from_millis(500),
            )
            .map_err(ProbeCreationError::Usb)?;

        interface
            .read_bulk(0x86, &mut ibuf, Duration::from_millis(500))
            .map_err(ProbeCreationError::Usb)?;

        Ok(Self {
            device: interface,
            speed_khz: 7500,
            epout,
            epin,
            pack,
            protocol: Some(WireProtocol::Swd),
            jtag_clocks: Vec::new(),
            jtag_bits: BitVec::new(),
        })
    }

    pub(crate) fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    // no chip manual, as i test low index with high speed, so 7 - index
    pub(crate) fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        let mut ibuf = [0; 10];
        if let Some(index) = speed_index(self.pack, speed_khz) {
            if self.protocol == Some(WireProtocol::Jtag) {
                self.write(&[0xD0, 0x06, 0x00, 0x00, index, 0x00, 0x00, 0x00, 0x00])
                    .unwrap();
            } else if self.protocol == Some(WireProtocol::Swd) {
                self.write(&[
                    0xe5,
                    0x08,
                    0x00,
                    0x40,
                    0x42,
                    0x0f,
                    0x00,
                    7 - index,
                    0x00,
                    0x00,
                    0x00,
                ])
                .unwrap();
            } else {
                panic!("No working protocol")
            }
            self.read(&mut ibuf).unwrap();
            Ok(speed_khz)
        } else {
            Err(DebugProbeError::UnsupportedSpeed(speed_khz))
        }
    }

    pub(crate) fn active_protocol(&self) -> Option<WireProtocol> {
        self.protocol
    }

    pub(crate) fn select_protocol(
        &mut self,
        protocol: WireProtocol,
    ) -> Result<(), DebugProbeError> {
        if self.protocol == Some(protocol) {
            Ok(())
        } else {
            self.protocol = Some(protocol);
            self.set_speed(self.speed_khz)?;
            Ok(())
        }
    }

    pub(crate) fn attach(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn write(&self, buf: &[u8]) -> Result<(), DebugProbeError> {
        self.device
            .write_bulk(self.epout, buf, Duration::from_millis(1000))
            .map_err(DebugProbeError::Usb)?;
        Ok(())
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, DebugProbeError> {
        let rev = self
            .device
            .read_bulk(self.epin, buf, Duration::from_millis(1000))
            .map_err(DebugProbeError::Usb)?;

        Ok(rev)
    }

    pub(crate) fn shift_bit(
        &mut self,
        tms: bool,
        tdi: bool,
        capture: bool,
    ) -> Result<(), DebugProbeError> {
        if self.jtag_clocks.len() >= 127 {
            self.jtag_flush()?;
        }
        self.jtag_clocks.push(JtagClock { tms, tdi, capture });
        Ok(())
    }

    fn jtag_flush(&mut self) -> Result<(), DebugProbeError> {
        let mut buffer = [0; 130];
        let mut obuf = vec![];
        let mut command = vec![0xD2];

        for &i in self.jtag_clocks.iter() {
            let byte = u8::from(i);
            // the byte is clock low, bit 0 = 1 that clock high
            obuf.push(byte);
            obuf.push(byte | 0x01);
        }
        command.extend_from_slice(&(obuf.len() as u16).to_le_bytes());
        command.extend_from_slice(&obuf);

        self.write(&command)?;
        self.read(&mut buffer)?;

        for (&c, &byte) in self.jtag_clocks.iter().zip(&buffer[3..]) {
            let JtagClock { capture, .. } = c;
            if capture {
                self.jtag_bits.push(byte != 0x00);
            }
        }

        self.jtag_clocks.clear();
        Ok(())
    }

    pub(crate) fn read_captured_bits(&mut self) -> Result<BitVec, DebugProbeError> {
        self.jtag_flush()?;
        Ok(std::mem::take(&mut self.jtag_bits))
    }

    // raw read, no bank
    pub(crate) fn read_reg(&self, address: RegisterAddress) -> Result<u32, u8> {
        let obuf = [
            0xe8,
            0x04,
            0x00,
            0xa2,
            0x22,
            0x00,
            gen_cmd(address.a2_and_3(), true, address.is_ap()),
        ];
        let mut ibuf = [0; 10];
        self.write(&obuf).unwrap();
        self.read(&mut ibuf).unwrap();
        let ack = ibuf[4];
        if ack != 1 {
            Err(ack)
        } else {
            let mut raw = [0; 4];
            raw.copy_from_slice(&ibuf[5..9]);
            Ok(u32::from_le_bytes(raw))
        }
    }

    // max is 72, over 72 it will read 2 times as a packet is 510 bytes
    pub(crate) fn batch_read_reg(
        &self,
        address: RegisterAddress,
        buf: &mut [u32],
    ) -> Result<(), ArmError> {
        let len = buf.len();
        assert!(len <= 72);

        let mut obuf = vec![0xE8];
        obuf.extend_from_slice(&((len * 4) as u16).to_le_bytes());
        let command = [
            0xA2,
            0x22,
            0x00,
            gen_cmd(address.a2_and_3(), true, address.is_ap()),
        ];
        for _ in 0..len {
            obuf.extend_from_slice(&command);
        }

        // send commands
        self.write(&obuf).unwrap();

        // a read with 1(0xA2) + 1(ACK) + 4(data) + 1(party+trn) bytes
        // all is 3 + 7 * len
        let mut ibuf = [0; 512];
        self.read(&mut ibuf).unwrap();
        let mut left = 3;
        for item in buf.iter_mut().take(len) {
            let ack = ibuf[left + 1];
            if ack != 1 {
                if ack == 7 {
                    return Err(ArmError::Dap(
                        crate::architecture::arm::DapError::NoAcknowledge,
                    ));
                } else if ack == 2 {
                    return Err(ArmError::Dap(
                        crate::architecture::arm::DapError::WaitResponse,
                    ));
                } else {
                    return Err(ArmError::Dap(
                        crate::architecture::arm::DapError::FaultResponse,
                    ));
                }
            }

            let mut raw = [0; 4];
            raw.copy_from_slice(&ibuf[left + 2..left + 6]);
            // if we should check parity at ibuf[left+6]
            *item = u32::from_le_bytes(raw);
            left += 7;
        }

        Ok(())
    }

    // max is 56
    pub(crate) fn batch_write_reg(
        &self,
        address: RegisterAddress,
        buf: &[u32],
    ) -> Result<(), ArmError> {
        let len = buf.len();
        assert!(len <= 56);

        let mut ibuf = [0; 512];
        let mut obuf = vec![0xE8];
        obuf.extend_from_slice(&((len * 9) as u16).to_le_bytes());

        let mut command = vec![
            0xA0,
            0x29,
            0x00,
            gen_cmd(address.a2_and_3(), false, address.is_ap()),
            0,
            0,
            0,
            0,
            0,
        ];

        for item in buf.iter().take(len) {
            (command[4..8]).copy_from_slice(&item.to_le_bytes());
            command[8] = (item.count_ones() % 2) as u8;

            obuf.extend_from_slice(&command);
        }

        self.write(&obuf).unwrap();
        let rev = self.read(&mut ibuf).unwrap();

        assert!(rev == 3 + len * 2);
        let mut left = 3;
        for _ in 0..len {
            let ack = ibuf[left + 1];
            if ack != 1 {
                if ack == 7 {
                    return Err(ArmError::Dap(
                        crate::architecture::arm::DapError::NoAcknowledge,
                    ));
                } else if ack == 2 {
                    return Err(ArmError::Dap(
                        crate::architecture::arm::DapError::WaitResponse,
                    ));
                } else {
                    return Err(ArmError::Dap(
                        crate::architecture::arm::DapError::FaultResponse,
                    ));
                }
            }

            left += 2;
        }

        Ok(())
    }

    pub(crate) fn jtag_seq(
        &mut self,
        cycles: u8,
        tms: u64,
        tdi: bool,
    ) -> Result<(), DebugProbeError> {
        for i in 0..cycles {
            self.shift_bit(tms >> i & 0x01 == 0x01, tdi, false)?;
        }

        self.jtag_flush()
    }

    pub(crate) fn swd_seq(&self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        let mut ibuf = [0; 4];
        let mut count = (bit_len / 8) as usize;
        let left = bit_len % 8;
        let mut seqs = bits.to_le_bytes();

        if left != 0 {
            let mut last = seqs[count];
            let bit = last >> (left - 1) & 0x01;

            for i in left..8 {
                last |= bit << i;
            }
            seqs[count] = last;
            count += 1;
        }

        let mut obuf = vec![0xE8];
        obuf.extend_from_slice(&(count as u16 + 3).to_le_bytes());
        obuf.push(0xA1);
        obuf.extend_from_slice(&(count as u16 * 8).to_le_bytes());
        obuf.extend_from_slice(&seqs[..count]);

        self.write(&obuf).unwrap();
        self.read(&mut ibuf).unwrap();
        Ok(())
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
                device.vendor_id(),
                device.product_id(),
                device.serial_number().map(Into::into),
                &Ch347UsbJtagFactory,
                None,
            )
        })
        .collect()
}

// generate a swd command header
fn gen_cmd(address: u8, is_read: bool, is_ap: bool) -> u8 {
    let cmd = 0b1000_0001
        | address << 1
        | if is_ap { 0x02 } else { 0x00 }
        | if is_read { 0x04 } else { 0x00 };

    let mut count = 0;
    for i in 1..=4 {
        if cmd >> i & 0x01 == 0x01 {
            count += 1;
        }
    }
    if count % 2 != 0 { cmd | 0x20 } else { cmd }
}

// actually as jtag index high with high speed
// but swd is low index with high speed
fn speed_index(pack: Pack, speed_khz: u32) -> Option<u8> {
    match pack {
        Pack::StandardPack => match speed_khz {
            1_875 => Some(0),
            3_750 => Some(1),
            7_500 => Some(2),
            15_000 => Some(3),
            30_000 => Some(4),
            60_000 => Some(5),
            _ => None,
        },
        Pack::LargePack => {
            match speed_khz {
                468 => Some(0), // 468.75 ~~ 468 kHz
                937 => Some(1), // 937.5  ~~ 937 kHz
                1_875 => Some(2),
                3_750 => Some(3),
                7_500 => Some(4),
                15_000 => Some(5),
                30_000 => Some(6),
                60_000 => Some(7),
                _ => None,
            }
        }
    }
}
