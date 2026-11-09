//! Texas Instruments XDS110 probe support, with the probe's own protocol.
//!
//! The XDS110 also has a CMSIS-DAP interface, which the CMSIS-DAP driver uses. The XDS110
//! protocol runs batches of up to 4 KB of SWD transfers or JTAG scans in one USB command, which
//! is several times faster.
//!
//! The firmware has no command for raw SWD sequences. They are sent on TMS as a path through
//! the JTAG state machine, which switches the probe to JTAG; the probe goes back to SWD before
//! the next transfer.

mod jtag;
mod swd;

use std::sync::Arc;
use std::time::Duration;

use nusb::{
    Endpoint, MaybeFuture,
    descriptors::TransferType,
    transfer::{Bulk, Direction, In, Out},
};

use crate::{
    Error,
    architecture::{
        arm::{
            ArmCommunicationInterface, ArmDebugInterface, ArmError, sequences::ArmDebugSequence,
        },
        riscv::{
            communication_interface::{RiscvError, RiscvInterfaceBuilder},
            dtm::jtag_dtm::JtagDtmBuilder,
        },
        xtensa::communication_interface::{
            XtensaCommunicationInterface, XtensaDebugInterfaceState, XtensaError,
        },
    },
    probe::{
        DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, JtagChain,
        JtagChainAccess, JtagChainState, ProbeCreationError, ProbeError, ProbeFactory, ResetLine,
        SwdProbe, WireProtocol,
        list::{ProbeListItem, usb_probe_accessibility},
        usb_util::{BulkReadExt, BulkWriteExt},
    },
};

/// USB IDs of the XDS110 configurations, with the interface of the XDS110 protocol.
const XDS110_DEVICES: &[(u16, u16, u8)] = &[
    (0x0451, 0xbef3, 2),
    (0x0451, 0xbef4, 2),
    (0x1cbe, 0x02a5, 0),
];

/// Largest batch of DAP requests or JTAG scans, in bytes, including the terminating zero.
const MAX_DATA_BLOCK: usize = 4096;

const TIMEOUT: Duration = Duration::from_secs(4);
/// The timeout of a CMAPI connect. After a TARGETSEL that deselected the debug port, the connect
/// takes about 7 s to fail.
const CMAPI_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The first firmware with batched DAP requests, JTAG scans and path moves.
const OCD_FIRMWARE_VERSION: u32 = 0x0203_0011;
/// The first firmware with fast TCK routines (up to 14 MHz).
const FAST_TCK_FIRMWARE_VERSION: u32 = 0x0300_0000;
/// The first firmware with the 10 and 12 MHz TCK settings.
const FAST_TCK_PLUS_FIRMWARE_VERSION: u32 = 0x0300_0003;

const XDS_CONNECT: u8 = 0x01;
const XDS_DISCONNECT: u8 = 0x02;
const XDS_VERSION: u8 = 0x03;
const XDS_SET_TCK: u8 = 0x04;
const XDS_SET_TRST: u8 = 0x05;
const XDS_CYCLE_TCK: u8 = 0x07;
const XDS_GOTO_STATE: u8 = 0x09;
const XDS_SET_SRST: u8 = 0x0e;
const SWD_CONNECT: u8 = 0x17;
const SWD_DISCONNECT: u8 = 0x18;
const CMAPI_CONNECT: u8 = 0x0f;
const CMAPI_DISCONNECT: u8 = 0x10;
const CJTAG_CONNECT: u8 = 0x2b;
const CJTAG_DISCONNECT: u8 = 0x2c;
const OCD_DAP_REQUEST: u8 = 0x3a;
const OCD_SCAN_REQUEST: u8 = 0x3b;
const OCD_PATHMOVE: u8 = 0x3c;

/// `CJTAG_CONNECT` format for 4-pin JTAG.
const MODE_JTAG: u32 = 1;

/// `XDS_GOTO_STATE` route: the quickest path.
const TRANSIT_QUICKEST: u32 = 1;

/// Errors specific to the XDS110 probe.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub(crate) enum Xds110Error {
    /// The XDS110 answered command {command:#04x} with error {code}.
    Command { command: u8, code: i32 },
    /// The XDS110 answered command {command:#04x} with {got} bytes, {expected} expected.
    ResponseLength {
        command: u8,
        got: usize,
        expected: usize,
    },
    /// The XDS110 sent a malformed response.
    MalformedResponse,
    /// The USB transfer to the XDS110 failed.
    Usb(#[source] std::io::Error),
    /// The XDS110 USB endpoints were not found.
    EndpointNotFound,
    /// The XDS110 firmware {0} is too old, version 2.3.0.11 or later is needed.
    FirmwareTooOld(String),
}

impl ProbeError for Xds110Error {}

/// Firmware error codes.
const SC_ERR_SWD_WAIT: i32 = -613;
const SC_ERR_SWD_FAULT: i32 = -614;
const SC_ERR_SWD_PROTOCOL: i32 = -615;
const SC_ERR_SWD_PARITY: i32 = -616;

/// Factory for creating [`Xds110`] probes.
#[derive(Debug)]
pub struct Xds110Factory;

impl std::fmt::Display for Xds110Factory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("XDS110")
    }
}

fn xds110_interface(device: &nusb::DeviceInfo) -> Option<u8> {
    XDS110_DEVICES
        .iter()
        .find(|(vid, pid, _)| device.vendor_id() == *vid && device.product_id() == *pid)
        .map(|(_, _, interface)| *interface)
}

/// The probe name, with the firmware version from the product string
/// ("XDS110 (03.00.00.45) Embed with CMSIS-DAP").
fn probe_name(device: &nusb::DeviceInfo) -> String {
    let product = device.product_string().unwrap_or("XDS110");
    match product.find(')') {
        Some(end) => product[..=end].to_string(),
        None => product.to_string(),
    }
}

impl ProbeFactory for Xds110Factory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;
        let (device, interface) = devices
            .filter_map(|device| xds110_interface(&device).map(|interface| (device, interface)))
            .find(|(device, interface)| {
                selector.interface.is_none_or(|i| i == *interface) && selector.matches(device)
            })
            .ok_or(ProbeCreationError::NotFound)?;

        let name = probe_name(&device);
        let mut usb = Xds110Usb::open(&device, interface)?;
        usb.execute(&[XDS_CONNECT], 0)?;
        let version = usb.execute(&[XDS_VERSION], 6)?;
        let firmware = u32::from_le_bytes(version[0..4].try_into().unwrap());
        let hardware = u16::from_le_bytes([version[4], version[5]]);
        tracing::info!(
            "XDS110: firmware {}, hardware {hardware:#06x}",
            firmware_string(firmware)
        );
        if firmware < OCD_FIRMWARE_VERSION {
            return Err(Xds110Error::FirmwareTooOld(firmware_string(firmware)).into());
        }

        Ok(Box::new(Xds110 {
            usb,
            name,
            firmware,
            // The default of the XDS110's CMSIS-DAP firmware.
            protocol: WireProtocol::Jtag,
            speed_khz: 2500,
            swd_connected: false,
            tms_state: None,
            jtag_state: JtagChainState::default(),
            reset_line: ResetLine::default(),
        }))
    }

    fn list_probes(&self) -> Vec<ProbeListItem> {
        let devices = match nusb::list_devices().wait() {
            Ok(devices) => devices,
            Err(e) => {
                tracing::warn!("listing XDS110 devices failed: {e}");
                return vec![];
            }
        };

        devices
            .filter_map(|device| {
                let interface = xds110_interface(&device)?;
                let info = DebugProbeInfo::new(
                    probe_name(&device),
                    device.vendor_id(),
                    device.product_id(),
                    device.serial_number().map(|s| s.to_string()),
                    &Xds110Factory,
                    Some(interface),
                    false,
                );
                Some(ProbeListItem {
                    info,
                    accessibility: usb_probe_accessibility(&device),
                })
            })
            .collect()
    }
}

/// The version is BCD coded, two digits per byte.
fn firmware_string(firmware: u32) -> String {
    let bytes = firmware.to_be_bytes();
    format!(
        "{:x}.{:x}.{:x}.{:x}",
        bytes[0], bytes[1], bytes[2], bytes[3]
    )
}

/// The bulk endpoints of the XDS110 protocol.
struct Xds110Usb {
    _interface: nusb::Interface,
    ep_out: Endpoint<Bulk, Out>,
    ep_in: Endpoint<Bulk, In>,
}

impl std::fmt::Debug for Xds110Usb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Xds110Usb").finish_non_exhaustive()
    }
}

impl Xds110Usb {
    fn open(device: &nusb::DeviceInfo, interface: u8) -> Result<Self, ProbeCreationError> {
        let device = device
            .open()
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;
        let configuration = device
            .active_configuration()
            .map_err(|_| ProbeCreationError::NotFound)?;
        let alt_setting = configuration
            .interface_alt_settings()
            .find(|alt| alt.interface_number() == interface)
            .ok_or(ProbeCreationError::NotFound)?;

        let mut ep_out = None;
        let mut ep_in = None;
        for endpoint in alt_setting.endpoints() {
            if endpoint.transfer_type() != TransferType::Bulk {
                continue;
            }
            match endpoint.direction() {
                Direction::Out => ep_out = ep_out.or(Some(endpoint.address())),
                Direction::In => ep_in = ep_in.or(Some(endpoint.address())),
            }
        }
        let (Some(ep_out), Some(ep_in)) = (ep_out, ep_in) else {
            return Err(Xds110Error::EndpointNotFound.into());
        };

        let interface = device
            .detach_and_claim_interface(interface)
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;
        let ep_out = interface
            .endpoint::<Bulk, Out>(ep_out)
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;
        let ep_in = interface
            .endpoint::<Bulk, In>(ep_in)
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;

        Ok(Self {
            _interface: interface,
            ep_out,
            ep_in,
        })
    }

    /// Sends one command and returns the `expected` bytes of the response after the error code.
    ///
    /// A packet is a '*', the length of the payload (two bytes) and the payload. A response
    /// payload starts with the error code (four bytes).
    fn execute(&mut self, payload: &[u8], expected: usize) -> Result<Vec<u8>, Xds110Error> {
        self.execute_with_timeout(payload, expected, TIMEOUT)
    }

    /// Like [`Self::execute`], with a timeout for each USB transfer.
    fn execute_with_timeout(
        &mut self,
        payload: &[u8],
        expected: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, Xds110Error> {
        tracing::trace!(
            "XDS110: command {:#04x}, {} bytes: {:02x?}",
            payload[0],
            payload.len(),
            &payload[..payload.len().min(32)]
        );
        let mut packet = Vec::with_capacity(payload.len() + 3);
        packet.push(b'*');
        packet.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        packet.extend_from_slice(payload);
        let written = self
            .ep_out
            .write_bulk(&packet, timeout)
            .map_err(Xds110Error::Usb)?;
        if written != packet.len() {
            return Err(Xds110Error::MalformedResponse);
        }

        let command = payload[0];
        let total = 3 + 4 + expected;
        let mut response = vec![0; total];
        let mut received = 0;
        while received < total {
            let n = self
                .ep_in
                .read_bulk(&mut response[received..], timeout)
                .map_err(Xds110Error::Usb)?;
            if received == 0 && (n < 7 || response[0] != b'*') {
                return Err(Xds110Error::MalformedResponse);
            }
            received += n;
            let size = u16::from_le_bytes([response[1], response[2]]) as usize;
            if n == 0 || received >= 3 + size {
                break;
            }
        }

        let size = u16::from_le_bytes([response[1], response[2]]) as usize;
        let code = i32::from_le_bytes(response[3..7].try_into().unwrap());
        if code != 0 {
            return Err(Xds110Error::Command { command, code });
        }
        if size != 4 + expected || received != total {
            return Err(Xds110Error::ResponseLength {
                command,
                got: size.saturating_sub(4),
                expected,
            });
        }
        response.drain(..7);
        Ok(response)
    }
}

/// A Texas Instruments XDS110 debug probe.
#[derive(Debug)]
pub struct Xds110 {
    usb: Xds110Usb,
    name: String,
    firmware: u32,
    protocol: WireProtocol,
    speed_khz: u32,
    /// Whether the probe is in SWD mode. A raw sequence leaves it in JTAG mode.
    swd_connected: bool,
    /// The TAP state after the last raw sequence, while nothing else moved the TAP.
    ///
    /// The next sequence continues from it: a sequence can be sent in several parts, such as
    /// the dormant wake-up, and no clocks may be added between them.
    tms_state: Option<jtag::XdsState>,
    jtag_state: JtagChainState,
    reset_line: ResetLine,
}

impl Xds110 {
    fn command(&mut self, payload: &[u8]) -> Result<(), DebugProbeError> {
        self.usb.execute(payload, 0)?;
        Ok(())
    }

    fn command_u32(&mut self, command: u8, value: u32) -> Result<(), DebugProbeError> {
        let mut payload = vec![command];
        payload.extend_from_slice(&value.to_le_bytes());
        self.command(&payload)
    }

    /// Puts the probe in SWD mode, unless it is already.
    fn swd_connect(&mut self) -> Result<(), DebugProbeError> {
        if !self.swd_connected {
            self.command(&[SWD_CONNECT])?;
            self.swd_connected = true;
            self.tms_state = None;
        }
        Ok(())
    }

    /// The TCK delay setting for a speed, and the speed it gives. The values come from
    /// OpenOCD's XDS110 driver.
    fn tck_delay(&self, speed_khz: u32) -> (u32, u32) {
        if self.firmware < FAST_TCK_FIRMWARE_VERSION {
            // About 66 ns per count, from 2.5 MHz.
            let speed_khz = speed_khz.clamp(100, 2500);
            let period_ns = 1_000_000 / speed_khz;
            let delay = (period_ns.saturating_sub(400) + 33) / 66;
            return (delay, 1_000_000 / (400 + 66 * delay));
        }
        let plus = self.firmware >= FAST_TCK_PLUS_FIRMWARE_VERSION;
        match speed_khz {
            14000.. => (0, 14000),
            12000.. if plus => (0xffff_fffe, 12000),
            10000.. if plus => (0xffff_fffd, 10000),
            8500.. => (1, 8500),
            5500.. => (2, 5500),
            _ => {
                // Measured by TI: delay = 17.1e6 / f - 1.02.
                let speed_khz = speed_khz.max(100);
                let delay = ((17_100_000.0 / (speed_khz as f64 * 1000.0) - 1.02) as u32).max(1);
                (
                    delay,
                    (17_100_000.0 / (delay as f64 + 1.02) / 1000.0) as u32,
                )
            }
        }
    }

    /// Sends TMS bits as a path through the JTAG state machine. The path starts where the last
    /// sequence ended, or else from Test-Logic-Reset. The probe is in JTAG mode afterwards.
    fn tms_sequence(
        &mut self,
        bits: impl Iterator<Item = bool>,
    ) -> Result<jtag::XdsState, DebugProbeError> {
        // In SWD mode, the probe does not drive TMS.
        if self.swd_connected {
            self.command(&[SWD_DISCONNECT])?;
            self.swd_connected = false;
        }
        let mut state = match self.tms_state {
            Some(state) => state,
            None => {
                self.goto_state(jtag::XdsState::Reset)?;
                jtag::XdsState::Reset
            }
        };
        let mut path = Vec::new();
        for tms in bits {
            state = state.next(tms);
            path.push(state as u8);
        }
        for chunk in path.chunks(256) {
            let mut payload = vec![OCD_PATHMOVE];
            payload.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            payload.extend_from_slice(chunk);
            self.command(&payload)?;
        }
        self.tms_state = Some(state);
        Ok(state)
    }

    fn goto_state(&mut self, state: jtag::XdsState) -> Result<(), DebugProbeError> {
        let mut payload = vec![XDS_GOTO_STATE];
        payload.extend_from_slice(&(state as u32).to_le_bytes());
        payload.extend_from_slice(&TRANSIT_QUICKEST.to_le_bytes());
        self.command(&payload)?;
        self.tms_state = Some(state);
        Ok(())
    }
}

impl DebugProbe for Xds110 {
    fn get_name(&self) -> &str {
        &self.name
    }

    fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        let (delay, actual) = self.tck_delay(speed_khz);
        self.command_u32(XDS_SET_TCK, delay)?;
        self.speed_khz = actual;
        Ok(actual)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Attaching with {} at {} kHz", self.protocol, self.speed_khz);
        self.set_speed(self.speed_khz)?;
        self.command(&[XDS_SET_TRST, 0])?;
        self.command_u32(XDS_CYCLE_TCK, 50)?;
        self.command(&[XDS_SET_TRST, 1])?;
        self.command_u32(XDS_CYCLE_TCK, 50)?;
        self.swd_connected = false;
        self.tms_state = None;
        match self.protocol {
            WireProtocol::Swd => {
                self.swd_connect()?;
                // Until a CMAPI connect ran since power-up, OCD_DAP_REQUEST hangs the firmware
                // until the probe is replugged, whether the target answers or not. The connect
                // reads DPIDR; it fails at once while the debug port does not answer, which is
                // enough.
                match self
                    .usb
                    .execute_with_timeout(&[CMAPI_CONNECT], 4, CMAPI_CONNECT_TIMEOUT)
                {
                    Ok(_) => self.command(&[CMAPI_DISCONNECT])?,
                    Err(Xds110Error::Command { code, .. }) => {
                        tracing::debug!("XDS110: CMAPI connect failed with error {code}");
                    }
                    Err(error) => return Err(error.into()),
                }
                Ok(())
            }
            WireProtocol::Jtag => {
                self.command_u32(CJTAG_CONNECT, MODE_JTAG)?;
                self.jtag_state.tap_state = crate::probe::TapState::TestLogicReset;
                self.goto_state(jtag::XdsState::Reset)
            }
        }
    }

    fn detach(&mut self) -> Result<(), Error> {
        if self.swd_connected {
            self.command(&[SWD_DISCONNECT])?;
            self.swd_connected = false;
        } else if self.protocol == WireProtocol::Jtag {
            self.command(&[CJTAG_DISCONNECT])?;
        }
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.target_reset_assert()?;
        std::thread::sleep(Duration::from_millis(10));
        self.target_reset_deassert()
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.reset_line.check_assert()?;
        self.command(&[XDS_SET_SRST, 0])
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.command(&[XDS_SET_SRST, 1])
    }

    fn set_reset_line_connected(&mut self, connected: bool) {
        self.reset_line.set_connected(connected);
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        self.protocol = protocol;
        Ok(())
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        Some(self.protocol)
    }

    fn has_arm_interface(&self) -> bool {
        true
    }

    fn try_get_arm_debug_interface<'probe>(
        self: Box<Self>,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Box<dyn ArmDebugInterface + 'probe>, (Box<dyn DebugProbe>, ArmError)> {
        let settings = SwdProbe::swd_settings(self.as_ref());
        Ok(match self.protocol {
            WireProtocol::Jtag => {
                ArmCommunicationInterface::create_jtag(self, settings, sequence, false)
            }
            WireProtocol::Swd => {
                ArmCommunicationInterface::create_swd(self, settings, sequence, false)
            }
        })
    }

    fn try_as_jtag_chain(&mut self) -> Option<JtagChain<'_>> {
        Some(JtagChain::new(self))
    }

    fn try_as_swd_probe(self: Box<Self>) -> Result<Box<dyn SwdProbe>, Box<dyn DebugProbe>> {
        match self.protocol {
            WireProtocol::Jtag => Err(self.into_probe()),
            _ => Ok(self),
        }
    }

    fn try_as_swd_probe_mut(&mut self) -> Option<&mut dyn SwdProbe> {
        Some(self)
    }

    fn try_as_jtag_chain_access_mut(&mut self) -> Option<&mut dyn JtagChainAccess> {
        Some(self)
    }

    fn has_riscv_interface(&self) -> bool {
        true
    }

    fn try_get_riscv_interface_builder<'probe>(
        &'probe mut self,
    ) -> Result<Box<dyn RiscvInterfaceBuilder<'probe> + 'probe>, RiscvError> {
        Ok(Box::new(JtagDtmBuilder::new(self)))
    }

    fn has_xtensa_interface(&self) -> bool {
        true
    }

    fn try_get_xtensa_interface<'probe>(
        &'probe mut self,
        state: &'probe mut XtensaDebugInterfaceState,
    ) -> Result<XtensaCommunicationInterface<'probe>, XtensaError> {
        Ok(XtensaCommunicationInterface::new(self, state))
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }
}

impl Drop for Xds110 {
    fn drop(&mut self) {
        let _ = self.usb.execute(&[XDS_DISCONNECT], 0);
    }
}
