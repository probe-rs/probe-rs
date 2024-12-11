//! Support for J-Link Debug probes

mod bits;
pub mod capabilities;
mod config;
mod connection;
mod error;
mod interface;
mod speed;
pub mod swo;

use std::fmt;
use std::iter;
use std::time::{Duration, Instant};

use bitvec::prelude::*;

use itertools::Itertools;
use nusb::transfer::{Direction, EndpointType};
use nusb::DeviceInfo;
use probe_rs_target::ScanChainElement;

use self::bits::BitIter;
use self::capabilities::{Capabilities, Capability};
use self::error::JlinkError;
use self::interface::{Interface, Interfaces};
use self::speed::SpeedConfig;
use self::swo::SwoMode;
use crate::architecture::arm::{ArmError, Pins};
use crate::architecture::xtensa::communication_interface::{
    XtensaCommunicationInterface, XtensaDebugInterfaceState,
};
use crate::probe::common::{JtagDriverState, RawJtagIo};
use crate::probe::jlink::bits::IteratorExt;
use crate::probe::jlink::config::JlinkConfig;
use crate::probe::jlink::connection::JlinkConnection;
use crate::probe::usb_util::InterfaceExt;
use crate::probe::JTAGAccess;
use crate::{
    architecture::{
        arm::{
            communication_interface::{DapProbe, UninitializedArmProbe},
            swo::SwoConfig,
            ArmCommunicationInterface, SwoAccess,
        },
        riscv::{communication_interface::RiscvInterfaceBuilder, dtm::jtag_dtm::JtagDtmBuilder},
    },
    probe::{
        arm_debug_interface::{ProbeStatistics, RawProtocolIo, SwdSettings},
        DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, ProbeFactory,
        WireProtocol,
    },
};

const SWO_BUFFER_SIZE: u16 = 128;
const TIMEOUT_DEFAULT: Duration = Duration::from_millis(500);

/// Factory to create [`JLink`] probes.
#[derive(Debug)]
pub struct JLinkFactory;

impl std::fmt::Display for JLinkFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("J-Link")
    }
}

impl ProbeFactory for JLinkFactory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        fn open_error(e: std::io::Error, while_: &'static str) -> DebugProbeError {
            let help = if cfg!(windows) {
                "(this error may be caused by not having the WinUSB driver installed; use Zadig (https://zadig.akeo.ie/) to install it for the J-Link device; this will replace the SEGGER J-Link driver)"
            } else {
                ""
            };

            DebugProbeError::Usb(std::io::Error::other(format!(
                "error while {while_}: {e}{help}",
            )))
        }

        let mut jlinks = nusb::list_devices()
            .map_err(DebugProbeError::Usb)?
            .filter(is_jlink)
            .filter(|info| selector.matches(info))
            .collect::<Vec<_>>();

        if jlinks.is_empty() {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                super::ProbeCreationError::NotFound,
            ));
        } else if jlinks.len() > 1 {
            tracing::warn!("More than one matching J-Link was found. Opening the first one.")
        }

        let info = jlinks.pop().unwrap();

        let handle = info
            .open()
            .map_err(|e| open_error(e, "opening the USB device"))?;

        let configs: Vec<_> = handle.configurations().collect();

        if configs.len() != 1 {
            tracing::warn!("device has {} configurations, expected 1", configs.len());
        }

        let conf = &configs[0];
        tracing::debug!("scanning {} interfaces", conf.interfaces().count());
        tracing::trace!("active configuration descriptor: {:#x?}", conf);

        let mut jlink_intf = None;
        for intf in conf.interfaces() {
            tracing::trace!("interface #{} descriptors:", intf.interface_number());

            for descr in intf.alt_settings() {
                tracing::trace!("{:#x?}", descr);

                // We detect the proprietary J-Link interface using the vendor-specific class codes
                // and the endpoint properties
                if descr.class() == 0xff && descr.subclass() == 0xff && descr.protocol() == 0xff {
                    if let Some((intf, _, _, _)) = jlink_intf {
                        Err(JlinkError::Other(format!(
                            "found multiple matching USB interfaces ({} and {})",
                            intf,
                            descr.interface_number()
                        )))?;
                    }

                    let endpoints: Vec<_> = descr.endpoints().collect();
                    tracing::trace!("endpoint descriptors: {:#x?}", endpoints);
                    if endpoints.len() != 2 {
                        tracing::warn!("vendor-specific interface with {} endpoints, expected 2 (skipping interface)", endpoints.len());
                        continue;
                    }

                    if !endpoints
                        .iter()
                        .all(|ep| ep.transfer_type() == EndpointType::Bulk)
                    {
                        tracing::warn!(
                            "encountered non-bulk endpoints, skipping interface: {:#x?}",
                            endpoints
                        );
                        continue;
                    }

                    let (read_ep, write_ep) = if endpoints[0].direction() == Direction::In {
                        (&endpoints[0], &endpoints[1])
                    } else {
                        (&endpoints[1], &endpoints[0])
                    };

                    jlink_intf = Some((
                        descr.interface_number(),
                        read_ep.address(),
                        write_ep.address(),
                        read_ep.max_packet_size(),
                    ));
                    tracing::debug!("J-Link interface is #{}", descr.interface_number());
                }
            }
        }

        let Some((intf, read_ep, write_ep, max_read_ep_packet)) = jlink_intf else {
            Err(JlinkError::Other(
                "device is not a J-Link device".to_string(),
            ))?
        };

        let handle = handle
            .claim_interface(intf)
            .map_err(|e| open_error(e, "taking control over USB device"))?;

        let mut this = JLink {
            read_ep,
            write_ep,
            max_read_ep_packet,
            caps: Capabilities::from_raw_legacy(0), // dummy value
            interface: Interface::Spi,              // dummy value, must not be JTAG
            interfaces: Interfaces::from_bits_warn(0), // dummy value
            handle,

            supported_protocols: vec![],  // dummy value
            protocol: WireProtocol::Jtag, // dummy value
            connection_handle: None,

            swo_config: None,
            speed_khz: 0, // default is unknown
            swd_settings: SwdSettings::default(),
            probe_statistics: ProbeStatistics::default(),
            jtag_state: JtagDriverState::default(),

            jtag_tms_bits: vec![],
            jtag_tdi_bits: vec![],
            jtag_capture_tdo: vec![],
            jtag_response: BitVec::new(),

            max_mem_block_size: 0, // dummy value
            jtag_chunk_size: 0,    // dummy value

            config: JlinkConfig::default(),
        };
        this.fill_capabilities()?;
        this.fill_interfaces()?;

        this.supported_protocols = if this.caps.contains(Capability::SelectIf) {
            this.interfaces
                .into_iter()
                .filter_map(|p| match WireProtocol::try_from(p) {
                    Ok(protocol) => Some(protocol),
                    Err(JlinkError::UnknownInterface(interface)) => {
                        // We ignore unknown protocols.
                        tracing::debug!(
                            "J-Link returned interface {:?}, which is not supported by probe-rs.",
                            interface
                        );
                        None
                    }
                    Err(_) => None,
                })
                .collect::<Vec<_>>()
        } else {
            // The J-Link cannot report which interfaces it supports, and cannot
            // switch interfaces. We assume it just supports JTAG.
            vec![WireProtocol::Jtag]
        };

        this.protocol = if this.supported_protocols.contains(&WireProtocol::Swd) {
            // Default to SWD if supported, since it's the most commonly used.
            WireProtocol::Swd
        } else {
            // Otherwise just pick the first supported.
            *this.supported_protocols.first().unwrap()
        };

        if this.caps.contains(Capability::GetMaxBlockSize) {
            this.max_mem_block_size = this.read_max_mem_block()?;

            tracing::debug!(
                "J-Link max mem block size for SWD IO: {} byte",
                this.max_mem_block_size
            );
        } else {
            tracing::debug!(
                "J-Link does not support GET_MAX_MEM_BLOCK, using default value of 65535"
            );
            this.max_mem_block_size = 65535;
        }

        // Some devices can't handle large transfers, so we limit the chunk size.
        // While it would be nice to read this directly from the device,
        // `read_max_mem_block`'s return value does not directly correspond to the
        // maximum transfer size when performing JTAG IO, and it's not clear how to get the actual value.
        // The number of *bits* is encoded as a u16, so the maximum value is 65535
        this.jtag_chunk_size = match selector.product_id {
            // 0x0101: J-Link EDU
            0x0101 => 65535,
            // 0x1051: J-Link OB-K22-SiFive: 504 bits
            0x1051 => 504,
            // Assume the lowest value is a safe default
            _ => 504,
        };
        this.config = this.read_device_config()?;
        this.connection_handle = if requires_connection_handle(selector) {
            Some(this.register_connection()?)
        } else {
            None
        };

        Ok(Box::new(this))
    }

    fn list_probes(&self) -> Vec<DebugProbeInfo> {
        list_jlink_devices()
    }
}

fn requires_connection_handle(selector: &DebugProbeSelector) -> bool {
    // These devices require a connection handle to be registered before they can be used.
    // As some other devices can't handle the registration command, we only enable it for known
    // devices.
    let devices = [
        (0x1366, 0x0101, Some("000000123456")), // Blue J-Link PRO clone
    ];

    devices.contains(&(
        selector.vendor_id,
        selector.product_id,
        selector.serial_number.as_deref(),
    ))
}

impl Drop for JLink {
    fn drop(&mut self) {
        self.unregister_connection().ok();
    }
}

#[repr(u8)]
#[allow(dead_code)]
enum Command {
    Version = 0x01,
    Register = 0x09,
    GetSpeeds = 0xC0,
    GetMaxMemBlock = 0xD4,
    GetCaps = 0xE8,
    GetCapsEx = 0xED,
    GetHwVersion = 0xF0,

    GetState = 0x07,
    GetHwInfo = 0xC1,
    GetCounters = 0xC2,
    MeasureRtckReact = 0xF6,

    ResetTrst = 0x02,
    SetSpeed = 0x05,
    SelectIf = 0xC7,
    SetKsPower = 0x08,
    HwClock = 0xC8,
    HwTms0 = 0xC9,
    HwTms1 = 0xCA,
    HwData0 = 0xCB,
    HwData1 = 0xCC,
    HwJtag = 0xCD,
    HwJtag2 = 0xCE,
    HwJtag3 = 0xCF,
    HwJtagWrite = 0xD5,
    HwJtagGetResult = 0xD6,
    HwTrst0 = 0xDE,
    HwTrst1 = 0xDF,
    Swo = 0xEB,
    WriteDcc = 0xF1,

    ResetTarget = 0x03,
    HwReleaseResetStopEx = 0xD0,
    HwReleaseResetStopTimed = 0xD1,
    HwReset0 = 0xDC,
    HwReset1 = 0xDD,
    GetCpuCaps = 0xE9,
    ExecCpuCmd = 0xEA,
    WriteMem = 0xF4,
    ReadMem = 0xF5,
    WriteMemArm79 = 0xF7,
    ReadMemArm79 = 0xF8,

    ReadConfig = 0xF2,
    WriteConfig = 0xF3,
}

/// A J-Link probe.
pub struct JLink {
    handle: nusb::Interface,

    read_ep: u8,
    write_ep: u8,
    max_read_ep_packet: usize,

    /// The capabilities reported by the device. They're fetched once, when the device is opened.
    caps: Capabilities,

    /// The supported interfaces. Like `caps`, this is fetched once when opening the device.
    interfaces: Interfaces,

    /// The currently selected target interface. This is stored here to avoid unnecessary roundtrips
    /// when performing target I/O operations.
    interface: Interface,

    /// Device configuration, fetched once when the device is opened.
    config: JlinkConfig,
    connection_handle: Option<u16>,

    swo_config: Option<SwoConfig>,

    /// Protocols supported by the probe.
    supported_protocols: Vec<WireProtocol>,
    /// Protocol chosen by the user
    protocol: WireProtocol,

    speed_khz: u32,

    jtag_tms_bits: Vec<bool>,
    jtag_tdi_bits: Vec<bool>,
    jtag_capture_tdo: Vec<bool>,
    jtag_response: BitVec<u8, Lsb0>,
    jtag_state: JtagDriverState,

    /// max number of bits in a transfer chunk, when using JTAG
    jtag_chunk_size: usize,

    /// Maximum memory block size, as report by the `GET_MAX_MEM_BLOCK` command.
    ///
    /// Used to determine maximum transfer length for SWD IO.
    max_mem_block_size: u32,

    probe_statistics: ProbeStatistics,
    swd_settings: SwdSettings,
}

impl fmt::Debug for JLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JLink").finish()
    }
}

impl JLink {
    /// Returns the supported J-Link capabilities.
    pub fn capabilities(&self) -> Capabilities {
        self.caps
    }

    /// Reads the advertised capabilities from the device.
    fn fill_capabilities(&mut self) -> Result<(), JlinkError> {
        self.write_cmd(&[Command::GetCaps as u8])?;

        let caps = self.read_u32().map(Capabilities::from_raw_legacy)?;

        tracing::debug!("legacy caps: {:?}", caps);

        // If the `GET_CAPS_EX` capability is set, use the extended capability command to fetch
        // all the capabilities.
        if caps.contains(Capability::GetCapsEx) {
            self.write_cmd(&[Command::GetCapsEx as u8])?;

            let real_caps = self.read_n::<32>().map(Capabilities::from_raw_ex)?;
            if !real_caps.contains_all(caps) {
                return Err(JlinkError::Other(format!(
                    "ext. caps are not a superset of legacy caps (legacy: {:?}, ex: {:?})",
                    caps, real_caps
                )));
            }
            tracing::debug!("extended caps: {:?}", real_caps);
            self.caps = real_caps;
        } else {
            tracing::debug!("extended caps not supported");
            self.caps = caps;
        }

        Ok(())
    }

    fn fill_interfaces(&mut self) -> Result<(), JlinkError> {
        if !self.caps.contains(Capability::SelectIf) {
            // Pre-SELECT_IF probes only support JTAG.
            self.interfaces = Interfaces::single(Interface::Jtag);
            self.interface = Interface::Jtag;

            return Ok(());
        }

        self.write_cmd(&[Command::SelectIf as u8, 0xFF])?;

        self.interfaces = self.read_u32().map(Interfaces::from_bits_warn)?;

        Ok(())
    }

    fn write_cmd(&self, cmd: &[u8]) -> Result<(), JlinkError> {
        tracing::trace!("write {} bytes: {:x?}", cmd.len(), cmd);

        let n = self
            .handle
            .write_bulk(self.write_ep, cmd, TIMEOUT_DEFAULT)?;

        if n != cmd.len() {
            return Err(JlinkError::Other(format!(
                "incomplete write (expected {} bytes, wrote {})",
                cmd.len(),
                n
            )));
        }
        Ok(())
    }

    fn read(&self, buf: &mut [u8]) -> Result<(), JlinkError> {
        let needs_workaround = buf.len() % self.max_read_ep_packet == 0;
        let len = buf.len();

        let mut tmp_buffer;
        let dst = if needs_workaround {
            // For some unknown reason, reading 256 bytes of config data leaves the interface in
            // an unusable state. Force-reading one more byte works around this issue.
            tmp_buffer = vec![0; len + 1];
            &mut tmp_buffer
        } else {
            tmp_buffer = vec![];
            &mut buf[..]
        };

        let mut total = 0;
        while total < len {
            let n = self
                .handle
                .read_bulk(self.read_ep, &mut dst[total..], TIMEOUT_DEFAULT)?;

            total += n;
        }

        if needs_workaround {
            buf.copy_from_slice(&tmp_buffer[..len]);
        }

        tracing::trace!("read {total} bytes: {buf:x?}");

        Ok(())
    }

    fn read_n<const N: usize>(&self) -> Result<[u8; N], JlinkError> {
        let mut buf = [0; N];
        self.read(&mut buf)?;
        Ok(buf)
    }

    fn read_u16(&self) -> Result<u16, JlinkError> {
        self.read_n::<2>().map(u16::from_le_bytes)
    }

    fn read_u32(&self) -> Result<u32, JlinkError> {
        self.read_n::<4>().map(u32::from_le_bytes)
    }

    fn require_capability(&self, cap: Capability) -> Result<(), JlinkError> {
        if self.caps.contains(cap) {
            Ok(())
        } else {
            Err(JlinkError::MissingCapability(cap))
        }
    }

    fn require_interface_supported(&self, intf: Interface) -> Result<(), JlinkError> {
        if self.interfaces.contains(intf) {
            Ok(())
        } else {
            Err(JlinkError::InterfaceNotSupported(intf))
        }
    }

    fn require_interface_selected(&self, intf: Interface) -> Result<(), JlinkError> {
        if self.interface == intf {
            Ok(())
        } else {
            Err(JlinkError::WrongInterfaceSelected {
                selected: self.interface,
                needed: intf,
            })
        }
    }

    /// Reads the maximum mem block size in Bytes.
    ///
    /// This requires the probe to support [`Capability::GetMaxBlockSize`].
    pub fn read_max_mem_block(&self) -> Result<u32, JlinkError> {
        // This cap refers to a nonexistent command `GET_MAX_BLOCK_SIZE`, but it probably means
        // `GET_MAX_MEM_BLOCK`.
        self.require_capability(Capability::GetMaxBlockSize)?;

        self.write_cmd(&[Command::GetMaxMemBlock as u8])?;

        self.read_u32()
    }

    fn read_device_config(&self) -> Result<JlinkConfig, JlinkError> {
        if self.caps.contains(Capability::ReadConfig) {
            self.write_cmd(&[Command::ReadConfig as u8])?;
            let bytes = self.read_n::<256>()?;

            let config = match JlinkConfig::parse(bytes) {
                Ok(config) => {
                    tracing::debug!("J-Link config: {:?}", config);
                    config
                }
                Err(error) => {
                    tracing::warn!("Failed to parse J-Link config: {error}");
                    JlinkConfig::default()
                }
            };

            Ok(config)
        } else {
            Ok(JlinkConfig::default())
        }
    }

    /// Reads the firmware version string from the device.
    fn read_firmware_version(&self) -> Result<String, JlinkError> {
        self.write_cmd(&[Command::Version as u8])?;

        let num_bytes = self.read_u16()?;
        let mut buf = vec![0; num_bytes as usize];
        self.read(&mut buf)?;

        Ok(String::from_utf8_lossy(
            // The firmware version string returned may contain null bytes. If
            // this happens, only return the preceding bytes.
            match buf.iter().position(|&b| b == 0) {
                Some(pos) => &buf[..pos],
                None => &buf,
            },
        )
        .into_owned())
    }

    /// Reads the hardware version from the device.
    ///
    /// This requires the probe to support [`Capability::GetHwVersion`].
    fn read_hardware_version(&self) -> Result<HardwareVersion, JlinkError> {
        self.require_capability(Capability::GetHwVersion)?;

        self.write_cmd(&[Command::GetHwVersion as u8])?;

        self.read_u32().map(HardwareVersion::from_u32)
    }

    /// Selects the interface to use for talking to the target MCU.
    ///
    /// Switching interfaces will reset the configured transfer speed, so [`JLink::set_speed`]
    /// needs to be called *after* `select_interface`.
    ///
    /// This requires the probe to support [`Capability::SelectIf`].
    ///
    /// **Note**: Selecting a different interface may cause the J-Link to perform target I/O!
    fn select_interface(&mut self, intf: Interface) -> Result<(), JlinkError> {
        if self.interface == intf {
            return Ok(());
        }

        self.require_capability(Capability::SelectIf)?;

        self.require_interface_supported(intf)?;

        self.write_cmd(&[Command::SelectIf as u8, intf as u8])?;

        // Returns the previous interface, ignore it
        let _ = self.read_u32()?;

        self.interface = intf;

        if self.speed_khz != 0 {
            // SelectIf resets the configured speed. Let's restore it.
            self.set_interface_clock_speed(SpeedConfig::khz(self.speed_khz as u16).unwrap())?;
        }

        Ok(())
    }

    /// Changes the state of the RESET pin (pin 15).
    ///
    /// RESET is an open-collector / open-drain output. If `reset` is `true`, the output will float.
    /// If `reset` is `false`, the output will be pulled to ground.
    ///
    /// **Note**: Some embedded J-Link probes may not expose this pin or may not allow controlling
    /// it using this function.
    fn set_reset(&mut self, reset: bool) -> Result<(), JlinkError> {
        let cmd = if reset {
            Command::HwReset1
        } else {
            Command::HwReset0
        };
        self.write_cmd(&[cmd as u8])
    }

    /// Resets the target's JTAG TAP controller by temporarily asserting (n)TRST (Pin 3).
    ///
    /// This might not do anything if the pin is not connected to the target. It does not affect
    /// non-JTAG target interfaces.
    fn reset_trst(&mut self) -> Result<(), JlinkError> {
        self.write_cmd(&[Command::ResetTrst as u8])
    }

    /// Reads the target voltage measured on the `VTref` pin, in millivolts.
    ///
    /// In order to use the J-Link, this voltage must be present, since it will be used as the level
    /// of the I/O signals to the target.
    fn read_target_voltage(&self) -> Result<u16, JlinkError> {
        self.write_cmd(&[Command::GetState as u8])?;

        let buf = self.read_n::<8>()?;

        let voltage = [buf[0], buf[1]];
        Ok(u16::from_le_bytes(voltage))
    }

    fn shift_jtag_bit(
        &mut self,
        tms: bool,
        tdi: bool,
        capture: bool,
    ) -> Result<(), DebugProbeError> {
        self.jtag_state.state.update(tms);

        self.jtag_tms_bits.push(tms);
        self.jtag_tdi_bits.push(tdi);
        self.jtag_capture_tdo.push(capture);

        if self.jtag_tms_bits.len() >= self.jtag_chunk_size {
            self.flush_jtag()?;
        }

        Ok(())
    }

    fn flush_jtag(&mut self) -> Result<(), JlinkError> {
        if self.jtag_tms_bits.is_empty() {
            return Ok(());
        }

        self.require_interface_selected(Interface::Jtag)?;

        let mut has_status_byte = false;
        // There's 3 commands for doing a JTAG transfer. The older 2 are obsolete with hardware
        // version 5 and above, which adds the 3rd command. Unfortunately we cannot reliably use the
        // HW version to determine this since some embedded J-Link probes have a HW version of
        // 1.0.0, but still support SWD, so we use the `SELECT_IF` capability instead.
        let cmd = if self.caps.contains(Capability::SelectIf) {
            // Use the new JTAG3 command, make sure to select the JTAG interface mode
            has_status_byte = true;
            Command::HwJtag3
        } else {
            // Use the legacy JTAG2 command
            // FIXME is HW_JTAG relevant at all?
            Command::HwJtag2
        };

        let tms_bit_count = self.jtag_tms_bits.len();
        let tdi_bit_count = self.jtag_tdi_bits.len();
        assert_eq!(
            tms_bit_count, tdi_bit_count,
            "TMS and TDI must have the same number of bits"
        );
        let capacity = 1 + 1 + 2 + tms_bit_count.div_ceil(8) * 2;
        let mut buf = Vec::with_capacity(capacity);
        buf.resize(4, 0);
        buf[0] = cmd as u8;
        // JTAG3 and JTAG2 use the same format for JTAG operations
        // buf[1] is dummy data for alignment
        // buf[2..=3] is the bit count
        let bit_count = u16::try_from(tms_bit_count).expect("too much data to transfer");
        buf[2..=3].copy_from_slice(&bit_count.to_le_bytes());
        buf.extend(self.jtag_tms_bits.drain(..).collapse_bytes());
        buf.extend(self.jtag_tdi_bits.drain(..).collapse_bytes());

        self.write_cmd(&buf)?;

        // Round bit count up to multple of 8 to get the number of response bytes.
        let num_resp_bytes = tms_bit_count.div_ceil(8);
        tracing::trace!(
            "{} TMS/TDI bits sent; reading {} response bytes",
            tms_bit_count,
            num_resp_bytes
        );

        // Response is `num_resp_bytes` TDO data bytes and one status byte,
        // if the JTAG3 command is used.
        let mut read_len = num_resp_bytes;

        if has_status_byte {
            read_len += 1;
        }

        self.read(&mut buf[..read_len])?;

        // Check the status if a JTAG3 command was used.
        if has_status_byte && buf[read_len - 1] != 0 {
            return Err(JlinkError::Other(format!(
                "probe I/O command returned error code {:#x}",
                buf[read_len - 1]
            )));
        }

        let response = BitIter::new(&buf[..num_resp_bytes], tms_bit_count);

        for (bit, capture) in response.zip(self.jtag_capture_tdo.drain(..)) {
            if capture {
                self.jtag_response.push(bit);
            }
        }

        Ok(())
    }

    fn read_captured_bits(&mut self) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        self.flush_jtag()?;

        Ok(std::mem::take(&mut self.jtag_response))
    }

    /// Perform a single SWDIO command
    ///
    /// The caller needs to ensure that the given iterators are not longer than the maximum transfer size
    /// allowed. It seems that the maximum transfer size is determined by [`self.max_mem_block_size`].
    fn perform_swdio_transfer<D, S>(&self, dir: D, swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        D: IntoIterator<Item = bool>,
        S: IntoIterator<Item = bool>,
    {
        self.require_interface_selected(Interface::Swd)?;

        const COMMAND_OVERHEAD: u32 = 4;

        let max_bits = ((self.max_mem_block_size - COMMAND_OVERHEAD) / 2 * 8) as usize;
        let max_bits = max_bits.min(65535);

        let dir_chunks = dir.into_iter().chunks(max_bits);
        let swdio_chunks = swdio.into_iter().chunks(max_bits);

        #[allow(clippy::useless_conversion)]
        let chunks = dir_chunks.into_iter().zip(swdio_chunks.into_iter());

        let mut output = Vec::new();
        let mut buf = Vec::with_capacity(self.max_mem_block_size as usize);
        buf.resize(4, 0);
        buf[0] = Command::HwJtag3 as u8;
        // buf[1] is dummy data for alignment
        buf[1] = 0;
        // buf[2..=3] is the bit count, which we'll fill in later

        for (dir, swdio) in chunks {
            buf.truncate(4);

            let mut dir_bit_count = 0;
            buf.extend(dir.inspect(|_| dir_bit_count += 1).collapse_bytes());
            let mut swdio_bit_count = 0;
            buf.extend(swdio.inspect(|_| swdio_bit_count += 1).collapse_bytes());

            assert_eq!(
                dir_bit_count, swdio_bit_count,
                "`dir` and `swdio` must have the same number of bits"
            );

            let num_bits = dir_bit_count as u16;
            buf[2..=3].copy_from_slice(&num_bits.to_le_bytes());
            let num_bytes = usize::from(num_bits.div_ceil(8));

            tracing::trace!("Buffer length for j-link transfer: {}", buf.len());

            self.write_cmd(&buf)?;

            // Response is `num_bytes` SWDIO data bytes and one status byte
            self.read(&mut buf[..num_bytes + 1])?;

            if buf[num_bytes] != 0 {
                return Err(JlinkError::Other(format!(
                    "probe I/O command returned error code {:#x}",
                    buf[num_bytes]
                ))
                .into());
            }

            output.extend(BitIter::new(&buf[..num_bytes], num_bits as usize));
        }

        Ok(output)
    }

    /// Enable/Disable the Target Power Supply of the probe.
    ///
    /// This is not available on all J-Links.
    pub fn set_kickstart_power(&mut self, enable: bool) -> Result<(), JlinkError> {
        self.require_capability(Capability::SetKsPower)?;
        self.write_cmd(&[Command::SetKsPower as u8, if enable { 1 } else { 0 }])
    }

    fn register_connection(&mut self) -> Result<u16, JlinkError> {
        if !self.caps.contains(Capability::Register) {
            return Ok(0);
        }

        // Undocumented, taken from OpenOCD/libjaylink
        let mut buf = vec![Command::Register as u8, 0x64];
        buf.extend(JlinkConnection::usb(0).into_bytes());
        self.write_cmd(&buf)?;

        let handle = self.read_registration_response()?;

        if handle == 0 {
            return Err(JlinkError::Other("Invalid registration handle".to_string()));
        }

        Ok(handle)
    }

    fn unregister_connection(&mut self) -> Result<(), JlinkError> {
        if !self.caps.contains(Capability::Register) {
            return Ok(());
        }

        if let Some(handle) = self.connection_handle.take() {
            let mut buf = vec![Command::Register as u8, 0x65];
            buf.extend(JlinkConnection::usb(handle).into_bytes());
            self.write_cmd(&buf)?;
            self.read_registration_response()?;
        }

        Ok(())
    }

    fn read_registration_response(&mut self) -> Result<u16, JlinkError> {
        const REG_HEADER_SIZE: usize = 8;
        const REG_MIN_SIZE: usize = 76;
        const REG_MAX_SIZE: usize = 512;

        let mut response = [0; REG_MAX_SIZE];
        self.read(&mut response[..REG_MIN_SIZE])?;

        let handle = u16::from_le_bytes([response[0], response[1]]);
        let num = u16::from_le_bytes([response[2], response[3]]) as usize;
        let entry_size = u16::from_le_bytes([response[4], response[5]]) as usize;
        let info_size = u16::from_le_bytes([response[6], response[7]]) as usize;

        let table_size = num * entry_size;
        let size = REG_HEADER_SIZE + table_size + info_size;

        tracing::debug!("Registration response size: {size}");

        if size > REG_MAX_SIZE {
            return Err(JlinkError::Other(format!(
                "Maximum registration size exceeded: {size} bytes",
            )));
        }

        if size > REG_MIN_SIZE {
            // Read the rest of the response.
            self.read(&mut response[REG_MIN_SIZE..size])?;
        }

        // TODO: we should process the response, and return the list of connections.

        Ok(handle)
    }
}

impl DebugProbe for JLink {
    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        if self.caps.contains(Capability::SelectIf) {
            let jlink_interface = match protocol {
                WireProtocol::Swd => Interface::Swd,
                WireProtocol::Jtag => Interface::Jtag,
            };

            if !self.interfaces.contains(jlink_interface) {
                return Err(DebugProbeError::UnsupportedProtocol(protocol));
            }
        } else {
            // Assume JTAG protocol if the probe does not support switching interfaces
            if protocol != WireProtocol::Jtag {
                return Err(DebugProbeError::UnsupportedProtocol(protocol));
            }
        }

        self.protocol = protocol;

        Ok(())
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        Some(self.protocol)
    }

    fn get_name(&self) -> &'static str {
        "J-Link"
    }

    fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    fn set_scan_chain(&mut self, scan_chain: Vec<ScanChainElement>) -> Result<(), DebugProbeError> {
        self.jtag_state.expected_scan_chain = Some(scan_chain);
        Ok(())
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        if speed_khz == 0 || speed_khz >= 0xffff {
            return Err(DebugProbeError::UnsupportedSpeed(speed_khz));
        }

        if let Ok(speeds) = self.read_interface_speeds() {
            tracing::debug!("Supported speeds: {:?}", speeds);

            let max_speed_khz = speeds.max_speed_hz() / 1000;

            if max_speed_khz < speed_khz {
                return Err(DebugProbeError::UnsupportedSpeed(speed_khz));
            }
        };

        if let Some(expected_speed) = SpeedConfig::khz(speed_khz as u16) {
            self.set_interface_clock_speed(expected_speed)?;
            self.speed_khz = speed_khz;
        } else {
            return Err(DebugProbeError::UnsupportedSpeed(speed_khz));
        }

        Ok(speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Attaching to J-Link");

        tracing::debug!("Attaching with protocol '{}'", self.protocol);

        if self.caps.contains(Capability::SelectIf) {
            let jlink_interface = match self.protocol {
                WireProtocol::Swd => Interface::Swd,
                WireProtocol::Jtag => Interface::Jtag,
            };

            self.select_interface(jlink_interface)?;
        }

        // Log some information about the probe
        tracing::debug!("J-Link: Capabilities: {:?}", self.caps);
        let fw_version = self.read_firmware_version().unwrap_or_else(|_| "?".into());
        tracing::info!("J-Link: Firmware version: {}", fw_version);
        match self.read_hardware_version() {
            Ok(hw_version) => tracing::info!("J-Link: Hardware version: {}", hw_version),
            Err(_) => tracing::info!("J-Link: Hardware version: ?"),
        };

        // Check and report the target voltage.
        let target_voltage = self.get_target_voltage()?.expect("The J-Link returned None when it should only be able to return Some(f32) or an error. Please report this bug!");
        if target_voltage < crate::probe::LOW_TARGET_VOLTAGE_WARNING_THRESHOLD {
            tracing::warn!(
                "J-Link: Target voltage (VTref) is {:2.2} V. Is your target device powered?",
                target_voltage
            );
        } else {
            tracing::info!("J-Link: Target voltage: {:2.2} V", target_voltage);
        }

        match self.protocol {
            WireProtocol::Jtag => {
                // try some JTAG stuff

                tracing::debug!("Resetting JTAG chain using trst");
                self.reset_trst()?;

                self.scan_chain()?;
                self.select_target(0)?;
            }
            WireProtocol::Swd => {
                // Attaching is handled in sequence

                // We are ready to debug.
            }
        }

        self.write_cmd(&[Command::HwReset1 as u8])?;
        self.write_cmd(&[Command::HwTrst1 as u8])?;

        // Set a default speed if not already set
        if self.speed_khz == 0 {
            self.set_speed(400)?;
        }

        tracing::debug!("Attached succesfully");

        Ok(())
    }

    fn select_jtag_tap(&mut self, index: usize) -> Result<(), DebugProbeError> {
        self.select_target(index)
    }

    fn scan_chain(&self) -> Result<&[ScanChainElement], DebugProbeError> {
        match self.active_protocol() {
            Some(WireProtocol::Jtag) => {
                if let Some(ref scan_chain) = self.jtag_state.expected_scan_chain {
                    Ok(scan_chain)
                } else {
                    Ok(&[])
                }
            }
            _ => Err(DebugProbeError::InterfaceNotAvailable {
                interface_name: "JTAG",
            }),
        }
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.write_cmd(&[Command::ResetTarget as u8])?;
        Ok(())
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.set_reset(false)?;
        Ok(())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.set_reset(true)?;
        Ok(())
    }

    fn try_get_riscv_interface_builder<'probe>(
        &'probe mut self,
    ) -> Result<Box<dyn RiscvInterfaceBuilder<'probe> + 'probe>, DebugProbeError> {
        if self.supported_protocols.contains(&WireProtocol::Jtag) {
            self.select_protocol(WireProtocol::Jtag)?;
            Ok(Box::new(JtagDtmBuilder::new(self)))
        } else {
            Err(DebugProbeError::InterfaceNotAvailable {
                interface_name: "JTAG",
            })
        }
    }

    fn get_swo_interface(&self) -> Option<&dyn SwoAccess> {
        Some(self as _)
    }

    fn get_swo_interface_mut(&mut self) -> Option<&mut dyn SwoAccess> {
        Some(self as _)
    }

    fn has_arm_interface(&self) -> bool {
        true
    }

    fn has_riscv_interface(&self) -> bool {
        self.supported_protocols.contains(&WireProtocol::Jtag)
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn try_as_dap_probe(&mut self) -> Option<&mut dyn DapProbe> {
        Some(self)
    }

    fn try_get_arm_interface<'probe>(
        self: Box<Self>,
    ) -> Result<Box<dyn UninitializedArmProbe + 'probe>, (Box<dyn DebugProbe>, DebugProbeError)>
    {
        let uninitialized_interface = ArmCommunicationInterface::new(self, true);

        Ok(Box::new(uninitialized_interface))
    }

    fn get_target_voltage(&mut self) -> Result<Option<f32>, DebugProbeError> {
        // Convert the integer millivolts value from self.handle to volts as an f32.
        Ok(Some((self.read_target_voltage()? as f32) / 1000f32))
    }

    fn try_get_xtensa_interface<'probe>(
        &'probe mut self,
        state: &'probe mut XtensaDebugInterfaceState,
    ) -> Result<XtensaCommunicationInterface<'probe>, DebugProbeError> {
        if self.supported_protocols.contains(&WireProtocol::Jtag) {
            self.select_protocol(WireProtocol::Jtag)?;
            Ok(XtensaCommunicationInterface::new(self, state))
        } else {
            Err(DebugProbeError::InterfaceNotAvailable {
                interface_name: "JTAG",
            })
        }
    }

    fn has_xtensa_interface(&self) -> bool {
        self.supported_protocols.contains(&WireProtocol::Jtag)
    }

    fn try_into_jlink(&mut self) -> Result<&mut JLink, DebugProbeError> {
        Ok(self)
    }
}

impl RawProtocolIo for JLink {
    fn jtag_shift_tms<M>(&mut self, tms: M, tdi: bool) -> Result<(), DebugProbeError>
    where
        M: IntoIterator<Item = bool>,
    {
        assert!(
            self.protocol != WireProtocol::Swd,
            "Logic error, requested jtag_io when in SWD mode"
        );

        self.probe_statistics.report_io();

        self.shift_bits(tms, iter::repeat(tdi), iter::repeat(false))?;

        Ok(())
    }

    fn jtag_shift_tdi<I>(&mut self, tms: bool, tdi: I) -> Result<(), DebugProbeError>
    where
        I: IntoIterator<Item = bool>,
    {
        assert!(
            self.protocol != WireProtocol::Swd,
            "Logic error, requested jtag_io when in SWD mode"
        );

        self.probe_statistics.report_io();

        self.shift_bits(iter::repeat(tms), tdi, iter::repeat(false))?;

        Ok(())
    }

    fn swd_io<D, S>(&mut self, dir: D, swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        D: IntoIterator<Item = bool>,
        S: IntoIterator<Item = bool>,
    {
        self.probe_statistics.report_io();
        self.perform_swdio_transfer(dir, swdio)
    }

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        let mut nreset = Pins(0);
        nreset.set_nreset(true);
        let nreset_mask = nreset.0 as u32;

        // If only the reset pin is selected we perform the reset.
        // If something else is selected return an error as this is not supported on J-Links.
        if pin_select == nreset_mask {
            if Pins(pin_out as u8).nreset() {
                self.target_reset_deassert()?;
            } else {
                self.target_reset_assert()?;
            }

            // Normally this would be the timeout we pass to the probe to settle the pins.
            // The J-Link is not capable of this, so we just wait for this time on the host
            // and assume it has settled until then.
            std::thread::sleep(Duration::from_micros(pin_wait as u64));

            // We signal that we cannot read the pin state.
            Ok(0xFFFF_FFFF)
        } else {
            // This is not supported for J-Links, unfortunately.
            Err(DebugProbeError::CommandNotSupportedByProbe {
                command_name: "swj_pins",
            })
        }
    }

    fn swd_settings(&self) -> &SwdSettings {
        &self.swd_settings
    }

    fn probe_statistics(&mut self) -> &mut ProbeStatistics {
        &mut self.probe_statistics
    }
}

impl RawJtagIo for JLink {
    fn state_mut(&mut self) -> &mut JtagDriverState {
        &mut self.jtag_state
    }

    fn state(&self) -> &JtagDriverState {
        &self.jtag_state
    }

    fn shift_bit(&mut self, tms: bool, tdi: bool, capture: bool) -> Result<(), DebugProbeError> {
        self.shift_jtag_bit(tms, tdi, capture)
    }

    fn read_captured_bits(&mut self) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        self.read_captured_bits()
    }
}

impl DapProbe for JLink {}

impl SwoAccess for JLink {
    fn enable_swo(&mut self, config: &SwoConfig) -> Result<(), ArmError> {
        self.swo_config = Some(*config);
        self.swo_start(SwoMode::Uart, config.baud(), SWO_BUFFER_SIZE.into())
            .map_err(DebugProbeError::from)?;
        Ok(())
    }

    fn disable_swo(&mut self) -> Result<(), ArmError> {
        self.swo_config = None;
        self.swo_stop().map_err(DebugProbeError::from)?;
        Ok(())
    }

    fn swo_buffer_size(&mut self) -> Option<usize> {
        Some(SWO_BUFFER_SIZE.into())
    }

    fn read_swo_timeout(&mut self, timeout: Duration) -> Result<Vec<u8>, ArmError> {
        let start = Instant::now();
        let mut buf = vec![0; SWO_BUFFER_SIZE.into()];

        let poll_interval = self
            .swo_poll_interval_hint(&self.swo_config.unwrap())
            .unwrap();

        let mut bytes = vec![];
        loop {
            let data = self.swo_read(&mut buf).map_err(DebugProbeError::from)?;
            bytes.extend(data.as_ref());
            if start.elapsed() > timeout {
                break;
            }
            std::thread::sleep(poll_interval);
        }
        Ok(bytes)
    }
}

#[tracing::instrument]
fn list_jlink_devices() -> Vec<DebugProbeInfo> {
    let Ok(devices) = nusb::list_devices() else {
        return vec![];
    };

    devices
        .filter(is_jlink)
        .map(|info| {
            DebugProbeInfo::new(
                info.product_string().unwrap_or("J-Link").to_string(),
                info.vendor_id(),
                info.product_id(),
                info.serial_number().map(|s| s.to_string()),
                &JLinkFactory,
                None,
            )
        })
        .collect()
}

impl TryFrom<Interface> for WireProtocol {
    type Error = JlinkError;

    fn try_from(interface: Interface) -> Result<Self, Self::Error> {
        match interface {
            Interface::Jtag => Ok(WireProtocol::Jtag),
            Interface::Swd => Ok(WireProtocol::Swd),
            unknown_interface => Err(JlinkError::UnknownInterface(unknown_interface)),
        }
    }
}

/// A hardware version returned by [`JLink::read_hardware_version`].
///
/// Note that the reported hardware version does not allow reliable feature detection, since
/// embedded J-Link probes might return a hardware version of 1.0.0 despite supporting SWD and other
/// much newer features.
#[derive(Debug)]
struct HardwareVersion(u32);

impl HardwareVersion {
    fn from_u32(raw: u32) -> Self {
        HardwareVersion(raw)
    }

    /// Returns the type of hardware (or `None` if the hardware type is unknown).
    fn hardware_type(&self) -> Option<HardwareType> {
        Some(match (self.0 / 1000000) % 100 {
            0 => HardwareType::JLink,
            1 => HardwareType::JTrace,
            2 => HardwareType::Flasher,
            3 => HardwareType::JLinkPro,
            _ => return None,
        })
    }

    /// The major version.
    fn major(&self) -> u8 {
        // Decimal coded Decimal, cool cool
        (self.0 / 10000) as u8
    }

    /// The minor version.
    fn minor(&self) -> u8 {
        ((self.0 % 10000) / 100) as u8
    }

    /// The hardware revision.
    fn revision(&self) -> u8 {
        (self.0 % 100) as u8
    }
}

impl fmt::Display for HardwareVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(hw) = self.hardware_type() {
            write!(f, "{} ", hw)?;
        }
        write!(f, "{}.{}.{}", self.major(), self.minor(), self.revision())
    }
}

/// The hardware/product type of the device.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum HardwareType {
    JLink,
    JTrace,
    Flasher,
    JLinkPro,
}

impl fmt::Display for HardwareType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HardwareType::JLink => "J-Link",
            HardwareType::JTrace => "J-Trace",
            HardwareType::Flasher => "J-Flash",
            HardwareType::JLinkPro => "J-Link Pro",
        })
    }
}

const VID_SEGGER: u16 = 0x1366;

fn is_jlink(info: &DeviceInfo) -> bool {
    info.vendor_id() == VID_SEGGER
}
