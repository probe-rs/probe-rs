//! WCH-LinkRV probe support.
//!
//! The protocl is mostly undocumented, and is changing between firmware versions.
//! For more details see: <https://github.com/ch32-rs/wlink>

use core::fmt;
use std::{thread::sleep, time::Duration};

use probe_rs_target::ScanChainElement;
use rusb::{Device, UsbContext};

use crate::{
    architecture::riscv::communication_interface::{RiscvCommunicationInterface, RiscvError},
    DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, DebugProbeType,
    ProbeCreationError, WireProtocol,
};

use self::usb_interface::WchLinkUsbDevice;
use super::JTAGAccess;

mod usb_interface;
mod commands;

const VENDOR_ID: u16 = 0x1a86;
const PRODUCT_ID: u16 = 0x8010;

// See: RISC-V Debug Specification, 6.1 JTAG DTM Registers
const DMI_VALUE_BIT_OFFSET: u32 = 2;
const DMI_ADDRESS_BIT_OFFSET: u32 = 34;
const DMI_OP_MASK: u128 = 0b11; // 2 bits

const DMI_OP_NOP: u8 = 0;
const DMI_OP_READ: u8 = 1;
const DMI_OP_WRITE: u8 = 2;

const REG_BYPASS_ADDRESS: u8 = 0x1f;
const REG_IDCODE_ADDRESS: u8 = 0x01;
const REG_DTMCS_ADDRESS: u8 = 0x10;
const REG_DMI_ADDRESS: u8 = 0x11;

const DTMCS_DMIRESET_MASK: u32 = 1 << 16;
const DTMCS_DMIHARDRESET_MASK: u32 = 1 << 17;

/// All WCH-Link probe variants, see-also: http://www.wch-ic.com/products/WCH-Link.html
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum WchLinkVariant {
    /// WCH-Link-CH549, does not support CH32V00X
    Ch549 = 1,
    /// WCH-LinkE-CH32V305
    ECh32v305 = 2,
    /// WCH-LinkS-CH32V203
    SCh32v203 = 3,
    /// WCH-LinkB,
    B = 4,
}

impl fmt::Display for WchLinkVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WchLinkVariant::Ch549 => write!(f, "WCH-Link-CH549"),
            WchLinkVariant::ECh32v305 => write!(f, "WCH-LinkE-CH32V305"),
            WchLinkVariant::SCh32v203 => write!(f, "WCH-LinkS-CH32V203"),
            WchLinkVariant::B => write!(f, "WCH-LinkB"),
        }
    }
}

/// Currently supported RISC-V chip series
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum RiscvChip {
    /// CH32V103 RISC-V3A series
    CH32V103 = 0x01,
    /// CH571/CH573 RISC-V3A BLE 4.2 series
    CH57x = 0x02,
    /// CH569/CH565 RISC-V3A series
    CH56x = 0x03,
    /// CH32V20x RISC-V4B/V4C series
    CH32V20x = 0x05,
    /// CH32V30x RISC-V4C/V4F series
    CH32V30x = 0x06,
    /// CH581/CH582/CH583 RISC-V4A BLE 5.3 series
    CH58x = 0x07,
    /// CH32V003 RISC-V2A series
    CH32V003 = 0x09,
}

impl RiscvChip {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(RiscvChip::CH32V103),
            0x02 => Some(RiscvChip::CH57x),
            0x03 => Some(RiscvChip::CH56x),
            0x05 => Some(RiscvChip::CH32V20x),
            0x06 => Some(RiscvChip::CH32V30x),
            0x07 => Some(RiscvChip::CH58x),
            0x09 => Some(RiscvChip::CH32V003),
            _ => None,
        }
    }
}

/// WCH-Link device (mod:RV)
#[derive(Debug)]
pub(crate) struct WchLink {
    device: WchLinkUsbDevice,
    name: String,
    variant: WchLinkVariant,
    v_major: u8,
    v_minor: u8,
    riscvchip: u8,
    chip_type: u32,
    // Hack to support NOP after READ
    last_dmi_read: Option<(u8, u32, u8)>,
}

impl WchLink {
    fn get_version(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Getting version of WCH-Link...");

        let rxbuf = self
            .device
            .write_command(&[0x81, 0x0d, 0x01, 0x01], Duration::from_millis(300))?;

        if rxbuf[0] != 0x82 {
            return Err(WchLinkError::CommandFailed.into());
        }

        match rxbuf.len() {
            // 2.3
            5 => {
                self.variant = WchLinkVariant::Ch549;
                self.v_major = rxbuf[3];
                self.v_minor = rxbuf[4];
                tracing::warn!(
                    "WCH-Link {} {}.{} is outdated, please update the firmware.",
                    self.variant,
                    self.v_major,
                    self.v_minor
                );
            }
            // 2.7, 2.8
            7 => {
                self.v_major = rxbuf[3];
                self.v_minor = rxbuf[4];

                match rxbuf[5] {
                    1 => self.variant = WchLinkVariant::Ch549,
                    2 => self.variant = WchLinkVariant::ECh32v305,
                    3 => self.variant = WchLinkVariant::SCh32v203,
                    4 => self.variant = WchLinkVariant::B,
                    _ => return Err(WchLinkError::UnknownDevice.into()),
                }
            }
            _ => return Err(WchLinkError::UnsupportedFirmwareVersion.into()),
        }

        Ok(())
    }

    fn init(&mut self) -> Result<(), DebugProbeError> {
        // first stage of wlink_init
        tracing::debug!("Initializing WCH-Link...");

        self.get_version()?;

        tracing::info!(
            "WCH-Link variant: {}, firmware version: {}.{}",
            self.variant,
            self.v_major,
            self.v_minor
        );

        if self.v_major != 0x02 && self.v_minor > 7 {
            return Err(WchLinkError::UnsupportedFirmwareVersion.into());
        }
        self.name = format!("{} v{}.{}", self.variant, self.v_major, self.v_minor);

        Ok(())
    }

    fn reset(&mut self) -> Result<(), DebugProbeError> {
        self.dmi_op_write(0x10, 0x80000001)?;
        sleep(Duration::from_millis(1));
        self.dmi_op_read(0x11)?;
        sleep(Duration::from_millis(1));

        let mut txbuf = [0x81, 0x0d, 0x01, 0x03];
        if self.riscvchip == 0x02 {
            txbuf[3] = 0x02;
        }
        self.device
            .write(&txbuf, &mut [0u8; 4], Duration::from_millis(300))?;

        self.dmi_op_write(0x10, 0x80000001)?;
        sleep(Duration::from_millis(1));
        self.dmi_op_read(0x11)?;
        Ok(())
    }

    fn dmi_op_read(&mut self, addr: u8) -> Result<(u8, u32, u8), DebugProbeError> {
        let mut rxbuf = [0u8; 9];
        self.device.write(
            &[0x81, 0x08, 0x06, addr, 0, 0, 0, 0, DMI_OP_READ],
            &mut rxbuf,
            Duration::from_millis(300),
        )?;

        if rxbuf[0] == 0x82 {
            let data_out = u32::from_be_bytes([rxbuf[4], rxbuf[5], rxbuf[6], rxbuf[7]]);
            Ok((rxbuf[3], data_out, rxbuf[8]))
        } else {
            Err(WchLinkError::CommandFailed.into())
        }
    }

    fn dmi_op_write(&mut self, addr: u8, data: u32) -> Result<(u8, u32, u8), DebugProbeError> {
        let mut rxbuf = [0u8; 9];
        let raw_data = data.to_be_bytes();
        self.device.write(
            &[
                0x81,
                0x08,
                0x06,
                addr,
                raw_data[0],
                raw_data[1],
                raw_data[2],
                raw_data[3],
                DMI_OP_WRITE,
            ],
            &mut rxbuf,
            Duration::from_millis(300),
        )?;

        if rxbuf[0] == 0x82 {
            let data_out = u32::from_be_bytes([rxbuf[4], rxbuf[5], rxbuf[6], rxbuf[7]]);
            Ok((rxbuf[3], data_out, rxbuf[8]))
        } else {
            Err(WchLinkError::CommandFailed.into())
        }
    }

    fn dmi_op_nop(&mut self, addr: u8, data: u32) -> Result<(u8, u32, u8), DebugProbeError> {
        let mut rxbuf = [0u8; 9];
        let raw_data = data.to_be_bytes();
        self.device.write(
            &[
                0x81,
                0x08,
                0x06,
                addr,
                raw_data[0],
                raw_data[1],
                raw_data[2],
                raw_data[3],
                DMI_OP_NOP,
            ],
            &mut rxbuf,
            Duration::from_millis(300),
        )?;

        let data_out = u32::from_be_bytes([rxbuf[4], rxbuf[5], rxbuf[6], rxbuf[7]]);
        if rxbuf[0] == 0x82 {
            Ok((rxbuf[3], data_out, rxbuf[8]))
        } else {
            Err(WchLinkError::CommandFailed.into())
        }
    }

    pub fn assert_unprotected(&mut self) -> Result<(), DebugProbeError> {
        let rxbuf = self
            .device
            .write_command(&[0x81, 0x06, 0x01, 0x01], Duration::from_millis(300))?;

        if rxbuf[0] == 0x82 {
            if rxbuf[3] == 0x02 {
                Ok(())
            } else if rxbuf[3] == 0x01 {
                Err(WchLinkError::ReadProtected.into())
            } else {
                unreachable!()
            }
        } else {
            Err(WchLinkError::CommandFailed.into())
        }
    }
}

impl DebugProbe for WchLink {
    fn new_from_selector(
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized,
    {
        let device = WchLinkUsbDevice::new_from_selector(selector)?;
        let mut wlink = Self {
            device,
            name: "WCH-Link".into(),
            variant: WchLinkVariant::Ch549,
            v_major: 0,
            v_minor: 0,
            chip_type: 0,
            riscvchip: 0,
            last_dmi_read: None,
        };

        wlink.init()?;

        Ok(Box::new(wlink))
    }

    fn get_name(&self) -> &str {
        &self.name
    }

    fn speed_khz(&self) -> u32 {
        todo!()
    }

    fn set_speed(&mut self, _speed_khz: u32) -> Result<u32, DebugProbeError> {
        Err(DebugProbeError::NotImplemented("set_speed"))
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        // second stage of wlink_init
        tracing::trace!("attach to target chip");
        let mut rxbuf = self
            .device
            .write_command(&[0x81, 0x0d, 0x01, 0x02], Duration::from_millis(300))?;
        if rxbuf[0] != 0x82 {
            return Err(WchLinkError::CommandFailed.into());
        }

        self.riscvchip = rxbuf[3];
        tracing::info!(
            "attach riscvchip 0x{:02x} {:?}",
            self.riscvchip,
            RiscvChip::from_u8(self.riscvchip)
        );

        if rxbuf.len() > 7 {
            let chip_type =
                u32::from_be_bytes([rxbuf[4], rxbuf[5], rxbuf[6], rxbuf[7]]) & 0xffffff0f;
            tracing::info!("attach chip_type 0x{:08x}", chip_type);
            self.chip_type = chip_type;
        } else {
            tracing::warn!("Using old firmware, chip_type not available")
        }

        match self.riscvchip {
            0x01 => {
                // CH32V103
                self.device.write(
                    &[0x81, 0x0d, 0x01, 0x03],
                    &mut rxbuf[0..4],
                    Duration::from_millis(300),
                )?;
                self.assert_unprotected()?;
                self.reset()?;
            }
            0x02 => {
                // CH57x
            }
            0x03 => {
                // CH569
                let rxbuf = self
                    .device
                    .write_command(&[0x81, 0x0d, 0x01, 0x04], Duration::from_millis(300))?;
                println!("rxbuf: {rxbuf:?}");
            }
            0x05 => {
                // CH32V20x
                self.device.write(
                    &[0x81, 0x0d, 0x01, 0x03],
                    &mut rxbuf[0..4],
                    Duration::from_millis(300),
                )?;
                self.assert_unprotected()?;
            }
            0x06 => {
                // CH32V30x
                self.device.write(
                    &[0x81, 0x0d, 0x01, 0x03],
                    &mut rxbuf[0..4],
                    Duration::from_millis(300),
                )?;
                self.assert_unprotected()?;
            }
            0x07 => {
                // CH58x
            }
            0x09 => {
                // CH32V003
                unimplemented!("CH32V003 (rv32ec) is not supported yet");
            }
            _ => unimplemented!("riscvchip 0x{:02}", self.riscvchip),
        }

        Ok(())
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        // wlink_disabledebug
        if self.riscvchip == 0x02 || self.riscvchip == 0x03 {
            self.device.write(
                &[0x81, 0x0e, 0x01, 0x00],
                &mut [0u8; 3],
                Duration::from_millis(300),
            )?;
        }
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::NotImplemented("target_reset"))
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        tracing::info!("target reset assert");
        Err(DebugProbeError::NotImplemented("target_reset_assert"))
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        tracing::info!("target reset deassert");
        Ok(())
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        // Assume Jtag, as it is the only supported protocol for riscv
        match protocol {
            WireProtocol::Jtag => Ok(()),
            _ => Err(DebugProbeError::UnsupportedProtocol(protocol)),
        }
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        Some(WireProtocol::Jtag)
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn has_riscv_interface(&self) -> bool {
        true
    }

    fn try_get_riscv_interface(
        self: Box<Self>,
    ) -> Result<RiscvCommunicationInterface, (Box<dyn DebugProbe>, RiscvError)> {
        RiscvCommunicationInterface::new(self).map_err(|(probe, err)| (probe.into_probe(), err))
    }

    fn set_scan_chain(
        &mut self,
        _scan_chain: Vec<ScanChainElement>,
    ) -> Result<(), DebugProbeError> {
        return Err(DebugProbeError::CommandNotSupportedByProbe(
            "Setting Scan Chain is not supported by WCH-LinkRV",
        ));
    }
}

/// Wrap WCH-Link's USB based DMI access as a fake JTAGAccess
impl JTAGAccess for WchLink {
    fn read_register(&mut self, address: u32, len: u32) -> Result<Vec<u8>, DebugProbeError> {
        tracing::debug!("read register 0x{:08x}", address);
        assert_eq!(len, 32);

        match address as u8 {
            REG_IDCODE_ADDRESS => {
                // using hard coded idcode 0x00000001, the same as WCH's official appro
                tracing::debug!("using hard coded idcode 0x00000001");
                Ok(0x1_u32.to_le_bytes().to_vec())
            }
            REG_DTMCS_ADDRESS => {
                // See: RISC-V Debug Specification, 6.1.4
                // 0x71: abits=7, version=1(1.0)
                Ok(0x71_u32.to_le_bytes().to_vec())
            }
            REG_BYPASS_ADDRESS => Ok(vec![0; 4]),
            _ => panic!("unknown read register address {address:08x}"),
        }
    }

    fn set_idle_cycles(&mut self, idle_cycles: u8) {
        tracing::debug!("set idle scycles {}, nop", idle_cycles);
    }

    fn get_idle_cycles(&self) -> u8 {
        todo!()
    }

    fn set_ir_len(&mut self, len: u32) {
        tracing::debug!("set ir len {}, nop", len);
    }

    fn write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
    ) -> Result<Vec<u8>, DebugProbeError> {
        match address as u8 {
            REG_DTMCS_ADDRESS => {
                let val = u32::from_le_bytes(data.try_into().unwrap());
                if val & DTMCS_DMIRESET_MASK != 0 {
                    tracing::warn!("TODO reset dmi");
                } else if val & DTMCS_DMIHARDRESET_MASK != 0 {
                    tracing::warn!("TODO hard reset dmi");
                }

                Ok(0x71_u32.to_le_bytes().to_vec())
            }
            REG_DMI_ADDRESS => {
                assert_eq!(
                    len, 41,
                    "should be 41 bits: 8 bits abits + 32 bits data + 2 bits op"
                );
                let register_value: u128 = u128::from_le_bytes(data.try_into().unwrap());

                let dmi_addr = ((register_value >> DMI_ADDRESS_BIT_OFFSET) & 0x3f) as u8;
                let dmi_value = ((register_value >> DMI_VALUE_BIT_OFFSET) & 0xffffffff) as u32;
                let dmi_op = (register_value & DMI_OP_MASK) as u8;

                tracing::trace!(
                    "dmi op={} addr 0x{:02x} data 0x{:08x}",
                    dmi_op,
                    dmi_addr,
                    dmi_value,
                );

                match dmi_op {
                    DMI_OP_READ => {
                        let (addr, data, op) = self.dmi_op_read(dmi_addr)?;
                        let ret = (addr as u128) << DMI_ADDRESS_BIT_OFFSET
                            | (data as u128) << DMI_VALUE_BIT_OFFSET
                            | (op as u128);
                        tracing::debug!("dmi read 0x{:02x} 0x{:08x} op={}", addr, data, op);
                        self.last_dmi_read = Some((addr, data, op));
                        Ok(ret.to_le_bytes().to_vec())
                    }
                    DMI_OP_NOP => {
                        // No idea why NOP with zero addr should return the last read value.
                        let (addr, data, op) = if dmi_addr == 0 && dmi_value == 0 {
                            self.dmi_op_nop(dmi_addr, dmi_value)?;
                            self.last_dmi_read.unwrap()
                        } else {
                            self.dmi_op_nop(dmi_addr, dmi_value)?
                        };

                        let ret = (addr as u128) << DMI_ADDRESS_BIT_OFFSET
                            | (data as u128) << DMI_VALUE_BIT_OFFSET
                            | (op as u128);
                        tracing::debug!("dmi nop 0x{:02x} 0x{:08x} op={}", addr, data, op);
                        Ok(ret.to_le_bytes().to_vec())
                    }
                    DMI_OP_WRITE => {
                        let (addr, data, op) = self.dmi_op_write(dmi_addr, dmi_value)?;
                        let ret = (addr as u128) << DMI_ADDRESS_BIT_OFFSET
                            | (data as u128) << DMI_VALUE_BIT_OFFSET
                            | (op as u128);
                        tracing::debug!("dmi write 0x{:02x} 0x{:08x} op={}", addr, data, op);
                        Ok(ret.to_le_bytes().to_vec())
                    }
                    _ => unreachable!("unknown dmi_op {dmi_op}"),
                }
            }
            _ => unreachable!("unknown register address 0x{:08x}", address),
        }
    }
}

fn get_wlink_info(device: &Device<rusb::Context>) -> Option<DebugProbeInfo> {
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

    if prod_str == "WCH-Link" {
        Some(DebugProbeInfo {
            identifier: "WCH-Link".into(),
            vendor_id: VENDOR_ID,
            product_id: PRODUCT_ID,
            serial_number: sn_str,
            probe_type: DebugProbeType::WchLink,
            hid_interface: None,
        })
    } else {
        None
    }
}

#[tracing::instrument(skip_all)]
pub fn list_wlink_devices() -> Vec<DebugProbeInfo> {
    tracing::debug!("Searching for WCH-Link probes using libusb");
    let probes = match rusb::Context::new().and_then(|ctx| ctx.devices()) {
        Ok(devices) => devices
            .iter()
            .filter(|device| {
                device
                    .device_descriptor()
                    .map(|desc| desc.vendor_id() == VENDOR_ID && desc.product_id() == PRODUCT_ID)
                    .unwrap_or(false)
            })
            .filter_map(|device| get_wlink_info(&device))
            .collect(),
        Err(_) => vec![],
    };

    tracing::debug!("Found {} WCH-Link probes total", probes.len());
    probes
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum WchLinkError {
    #[error("Device's read-protect status is enabled")]
    ReadProtected,
    #[error("Unknown WCH-Link device(new variant?)")]
    UnknownDevice,
    #[error("Firmware version is not supported.")]
    UnsupportedFirmwareVersion,
    #[error("Not enough bytes written.")]
    NotEnoughBytesWritten { is: usize, should: usize },
    #[error("Not enough bytes read.")]
    NotEnoughBytesRead { is: usize, should: usize },
    #[error("Usb endpoint not found.")]
    EndpointNotFound,
    #[error("Command failed from the device.")]
    CommandFailed,
}

impl From<WchLinkError> for DebugProbeError {
    fn from(e: WchLinkError) -> Self {
        DebugProbeError::ProbeSpecific(Box::new(e))
    }
}

impl From<WchLinkError> for ProbeCreationError {
    fn from(e: WchLinkError) -> Self {
        ProbeCreationError::ProbeSpecific(Box::new(e))
    }
}
