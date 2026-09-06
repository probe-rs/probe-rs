//! WCH-LinkRV probe support.
//!
//! The protocol is mostly undocumented, and is changing between firmware versions.
//! For more details see: <https://github.com/ch32-rs/wlink>

use std::fmt;

use nusb::{DeviceInfo, MaybeFuture};

use self::{commands::Speed, usb_interface::WchLinkUsbDevice};
use crate::{
    architecture::riscv::{
        communication_interface::{RiscvError, RiscvInterfaceBuilder},
        dtm::wlink_dtm::WchLinkDtmBuilder,
    },
    probe::{
        DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, ProbeError, ProbeFactory,
        WireProtocol,
        list::{ProbeListItem, usb_probe_accessibility},
    },
};

mod commands;
mod usb_interface;

const VENDOR_ID: u16 = 0x1a86;
const PRODUCT_ID: u16 = 0x8010;

// See: RISC-V Debug Specification, 6.1 JTAG DTM Registers
pub(crate) const DMI_OP_NOP: u8 = 0;
pub(crate) const DMI_OP_READ: u8 = 1;
pub(crate) const DMI_OP_WRITE: u8 = 2;

/// All WCH-Link probe variants, see-also: <http://www.wch-ic.com/products/WCH-Link.html>
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WchLinkVariant {
    /// WCH-Link-CH549, does not support RV32EC
    Ch549 = 1,
    /// WCH-LinkE-CH32V305, the full featured version
    ECh32v305 = 2,
    /// WCH-LinkS-CH32V203
    SCh32v203 = 3,
    /// WCH-LinkW-CH32V208, a wirelessed version
    WCh32v208 = 5,
}

impl fmt::Display for WchLinkVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WchLinkVariant::Ch549 => write!(f, "WCH-Link-CH549"),
            WchLinkVariant::ECh32v305 => write!(f, "WCH-LinkE-CH32V305"),
            WchLinkVariant::SCh32v203 => write!(f, "WCH-LinkS-CH32V203"),
            WchLinkVariant::WCh32v208 => write!(f, "WCH-LinkW-CH32V208"),
        }
    }
}

impl WchLinkVariant {
    fn try_from_u8(value: u8) -> Result<Self, WchLinkError> {
        match value {
            1 => Ok(Self::Ch549),
            2 | 0x12 => Ok(Self::ECh32v305),
            3 => Ok(Self::SCh32v203),
            5 | 0x85 => Ok(Self::WCh32v208),
            _ => Err(WchLinkError::UnknownDevice),
        }
    }
}

/// Currently supported RISC-V chip series/families. The IP core name is "Qingke".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RiscvChip {
    /// CH32V103 Qingke-V3A series
    CH32V103 = 0x01,
    /// CH571/CH573 Qingke-V3A BLE 4.2 series
    CH57X = 0x02,
    /// CH565/CH569 Qingke-V3A series
    CH56X = 0x03,
    /// CH32F10X Cortex-M3 series
    CH32F10X = 0x04,
    /// CH32V20X Qingke-V4B/V4C series
    CH32V20X = 0x05,
    /// CH32V30X Qingke-V4C/V4F series, the same as CH32V20X
    CH32V30X = 0x06,
    /// CH58x Qingke-V4A BLE 5.3 series
    CH58X = 0x07,
    /// CH32V003 Qingke-V2A series
    CH32V003 = 0x09,
    // The only reference I can find is <https://www.wch.cn/news/606.html>.
    /// RISC-V EC controller, undocumented.
    CH8571 = 0x0A, // 10,
    /// CH59x Qingke-V4C BLE 5.4 series, fallback as CH58X
    CH59X = 0x0B, // 11
    /// CH643 Qingke-V4C series, RGB Display Driver MCU
    CH643 = 0x0C, // 12
    /// CH32X035 Qingke-V4C USB-PD series, fallback as CH643
    CH32X035 = 0x0D, // 13
    /// CH32L103 Qingke-V4C low power series, USB-PD
    CH32L103 = 0x0E, // 14
    /// CH641 Qingke-V2A series, USB-PD, fallback as CH32V003
    CH641 = 0x49,
    /// CH32V00X Qingke-V2C series (V002/V004/V005/V006/V007)
    CH32V00X = 0x4e,
    /// CH32V317 Qingke-V4F gigabit Ethernet series
    CH32V317 = 0x86,
    /// CH570/CH572 Qingke-V3C series
    CH570 = 0x8B,
    /// CH32H4 Qingke-V4F high-performance series (H415/H416/H417)
    CH32H4 = 0xC6,
}

impl RiscvChip {
    fn try_from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(RiscvChip::CH32V103),
            0x02 => Some(RiscvChip::CH57X),
            0x03 => Some(RiscvChip::CH56X),
            0x04 => Some(RiscvChip::CH32F10X),
            0x05 => Some(RiscvChip::CH32V20X),
            0x06 => Some(RiscvChip::CH32V30X),
            0x07 => Some(RiscvChip::CH58X),
            0x09 => Some(RiscvChip::CH32V003),
            0x0A => Some(RiscvChip::CH8571),
            0x0B => Some(RiscvChip::CH59X),
            0x0C => Some(RiscvChip::CH643),
            0x0D => Some(RiscvChip::CH32X035),
            0x0E => Some(RiscvChip::CH32L103),
            0x49 => Some(RiscvChip::CH641),
            0x4E => Some(RiscvChip::CH32V00X),
            0x86 => Some(RiscvChip::CH32V317),
            0x8B => Some(RiscvChip::CH570),
            0xC6 => Some(RiscvChip::CH32H4),
            _ => None,
        }
    }

    fn support_flash_protect(&self) -> bool {
        matches!(
            self,
            RiscvChip::CH32V103
                | RiscvChip::CH32V20X
                | RiscvChip::CH32V30X
                | RiscvChip::CH32V003
                | RiscvChip::CH32V00X
                | RiscvChip::CH643
                | RiscvChip::CH32L103
                | RiscvChip::CH32X035
                | RiscvChip::CH641
                | RiscvChip::CH32V317
                | RiscvChip::CH32H4
        )
    }
}

/// Factory for creating [`WchLink`] probes.
#[derive(Debug)]
pub struct WchLinkFactory;

impl std::fmt::Display for WchLinkFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WchLink")
    }
}

impl ProbeFactory for WchLinkFactory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        let device = WchLinkUsbDevice::new_from_selector(selector)?;
        let mut wlink = WchLink {
            device,
            name: "WCH-Link".into(),
            variant: WchLinkVariant::Ch549,
            v_major: 0,
            v_minor: 0,
            chip_id: 0,
            chip_family: RiscvChip::CH32V103,
            last_dmi_read: None,
            speed: Speed::default(),
        };

        wlink.init()?;

        Ok(Box::new(wlink))
    }

    fn list_probes(&self) -> Vec<ProbeListItem> {
        list_wlink_devices()
    }
}

/// A WCH-Link device (mod:RV)
pub struct WchLink {
    device: WchLinkUsbDevice,
    name: String,
    variant: WchLinkVariant,
    v_major: u8,
    v_minor: u8,
    /// Chip family
    chip_family: RiscvChip,
    /// Chip id to identify the target chip variant
    chip_id: u32,
    // Hack to support NOP after READ
    last_dmi_read: Option<(u8, u32, u8)>,
    speed: commands::Speed,
}

impl fmt::Debug for WchLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WchLink")
            .field("name", &self.name)
            .field("variant", &self.variant)
            .field("v_major", &self.v_major)
            .field("v_minor", &self.v_minor)
            .field("chip_family", &self.chip_family)
            .field("chip_id", &self.chip_id)
            .field("last_dmi_read", &self.last_dmi_read)
            .field("speed", &self.speed)
            .finish()
    }
}

impl WchLink {
    fn get_probe_info(&mut self) -> Result<(), DebugProbeError> {
        let probe_info = self.device.send_command(commands::GetProbeInfo)?;
        self.v_major = probe_info.major_version;
        self.v_minor = probe_info.minor_version;

        if self.v_major != 0x02 && self.v_minor < 0x07 {
            return Err(WchLinkError::UnsupportedFirmwareVersion("2.7").into());
        }

        self.variant = probe_info.variant;

        Ok(())
    }

    fn init(&mut self) -> Result<(), DebugProbeError> {
        // first stage of wlink_init
        tracing::debug!("Initializing WCH-Link...");

        self.get_probe_info()?;

        // this is the official version format. So "v31" is actually a 2.11
        let version_code = self.v_major * 10 + self.v_minor;

        tracing::info!(
            "WCH-Link variant: {}, firmware version: {}.{} (v{})",
            self.variant,
            self.v_major,
            self.v_minor,
            version_code
        );

        if self.v_major != 0x02 && self.v_minor < 0x7 {
            return Err(WchLinkError::UnsupportedFirmwareVersion("2.7").into());
        }
        self.name = format!("{} v{}.{}", self.variant, self.v_major, self.v_minor);

        Ok(())
    }

    /// `chip_id` reported by the probe's `AttachChip`; zero before attach.
    pub fn chip_id(&self) -> u32 {
        self.chip_id
    }

    /// Chip family reported by the probe's `AttachChip`.
    pub fn chip_family(&self) -> RiscvChip {
        self.chip_family
    }

    pub(crate) fn dmi_op_read(&mut self, addr: u8) -> Result<(u8, u32, u8), DebugProbeError> {
        let resp = self.device.send_command(commands::DmiOp::read(addr))?;

        Ok((resp.addr, resp.data, resp.op))
    }

    pub(crate) fn dmi_op_write(
        &mut self,
        addr: u8,
        data: u32,
    ) -> Result<(u8, u32, u8), DebugProbeError> {
        let resp = self
            .device
            .send_command(commands::DmiOp::write(addr, data))?;

        Ok((resp.addr, resp.data, resp.op))
    }

    pub(crate) fn dmi_op_nop(&mut self) -> Result<(u8, u32, u8), DebugProbeError> {
        let resp = self.device.send_command(commands::DmiOp::nop())?;

        Ok((resp.addr, resp.data, resp.op))
    }

    pub(crate) fn last_dmi_read(&self) -> Option<(u8, u32, u8)> {
        self.last_dmi_read
    }

    pub(crate) fn set_last_dmi_read(&mut self, value: (u8, u32, u8)) {
        self.last_dmi_read = Some(value);
    }
}

impl DebugProbe for WchLink {
    fn get_name(&self) -> &str {
        &self.name
    }

    fn speed_khz(&self) -> u32 {
        self.speed.to_khz()
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        let speed =
            Speed::from_khz(speed_khz).ok_or(DebugProbeError::UnsupportedSpeed(speed_khz))?;
        self.speed = speed;
        self.device
            .send_command(commands::SetSpeed(self.chip_family, speed))?;
        Ok(speed.to_khz())
    }

    /// Attach chip
    fn attach(&mut self) -> Result<(), DebugProbeError> {
        // second stage of wlink_init
        tracing::trace!("attach to target chip");

        self.device
            .send_command(commands::SetSpeed(self.chip_family, self.speed))?;

        let resp = self.device.send_command(commands::AttachChip)?;

        self.chip_family = resp.chip_family;

        self.chip_id = resp.chip_id;

        tracing::info!(
            "attached riscv chip {:?} with chip id 0x{:08x}",
            self.chip_family,
            self.chip_id
        );

        if self.chip_family.support_flash_protect() {
            self.device.send_command(commands::CheckFlashProtection)?;
            self.device.send_command(commands::UnprotectFlash)?;
        }

        Ok(())
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        tracing::trace!("Detach chip");
        self.device.send_command(commands::DetachChip)?;

        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.device.send_command(commands::ResetTarget)?;
        Ok(())
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        tracing::info!("target reset assert");
        self.device
            .send_command(commands::DmiOp::write(0x10, 0x80000001))?;
        Ok(())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        tracing::info!("target reset deassert");
        self.device
            .send_command(commands::DmiOp::write(0x10, 0x00000001))?;
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

    fn try_as_jtag_probe(&mut self) -> Option<&mut dyn crate::probe::JtagAccess> {
        None
    }

    fn try_get_riscv_interface_builder<'probe>(
        &'probe mut self,
    ) -> Result<Box<dyn RiscvInterfaceBuilder<'probe> + 'probe>, RiscvError> {
        Ok(Box::new(WchLinkDtmBuilder::new(self)))
    }
}

fn get_wlink_info(device: &DeviceInfo) -> Option<ProbeListItem> {
    if matches!(device.product_string(), Some("WCH-Link") | Some("WCH_Link")) {
        let info = DebugProbeInfo::new(
            "WCH-Link",
            VENDOR_ID,
            PRODUCT_ID,
            device.serial_number().map(|s| s.to_string()),
            &WchLinkFactory,
            None,
            false,
        );
        Some(ProbeListItem {
            info,
            accessibility: usb_probe_accessibility(device),
        })
    } else {
        None
    }
}

#[tracing::instrument(skip_all)]
fn list_wlink_devices() -> Vec<ProbeListItem> {
    tracing::debug!("Searching for WCH-Link(RV) probes");
    let devices = match nusb::list_devices().wait() {
        Ok(devices) => devices,
        Err(e) => {
            tracing::warn!("error listing WCH-Link devices: {e}");
            return vec![];
        }
    };
    let probes: Vec<_> = devices
        .filter(|device| device.vendor_id() == VENDOR_ID && device.product_id() == PRODUCT_ID)
        .filter_map(|device| get_wlink_info(&device))
        .collect();

    tracing::debug!("Found {} WCH-Link probes total", probes.len());
    probes
}

#[derive(thiserror::Error, Debug, docsplay::Display)]
pub(crate) enum WchLinkError {
    /// Unknown WCH-Link device.
    UnknownDevice,
    /// The firmware on the probe is outdated, and not supported by probe-rs. The minimum supported firmware version is {0}.
    UnsupportedFirmwareVersion(&'static str),
    /// Not enough bytes written.
    NotEnoughBytesWritten { is: usize, should: usize },
    /// Not enough bytes read.
    NotEnoughBytesRead { is: usize, should: usize },
    /// Usb endpoint not found.
    EndpointNotFound,
    /// Invalid payload.
    InvalidPayload,
    /// Protocol error.
    Protocol(u8, Vec<u8>),
    /// Unknown chip {0:#04x}.
    UnknownChip(u8),
}

impl ProbeError for WchLinkError {}
