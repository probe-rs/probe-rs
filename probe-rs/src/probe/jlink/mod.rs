//! Support for J-Link Debug probes

mod jaylink;

use bitvec::prelude::*;
use bitvec::vec::BitVec;
use jaylink::{Capability, Interface, JayLink, SpeedConfig, SwoMode};
use probe_rs_target::ScanChainElement;

use std::convert::TryFrom;
use std::iter;

use crate::architecture::arm::ArmError;
use crate::architecture::riscv::communication_interface::RiscvError;
use crate::architecture::xtensa::communication_interface::XtensaCommunicationInterface;
use crate::probe::common::{JtagDriverState, RawJtagIo};
use crate::probe::JTAGAccess;
use crate::probe::ProbeDriver;
use crate::{
    architecture::{
        arm::{
            communication_interface::DapProbe, communication_interface::UninitializedArmProbe,
            swo::SwoConfig, ArmCommunicationInterface, SwoAccess,
        },
        riscv::communication_interface::RiscvCommunicationInterface,
    },
    probe::{
        arm_jtag::{ProbeStatistics, RawProtocolIo, SwdSettings},
        DebugProbe, DebugProbeError, DebugProbeInfo, WireProtocol,
    },
    DebugProbeSelector,
};

const SWO_BUFFER_SIZE: u16 = 128;

pub struct JLinkSource;

impl std::fmt::Debug for JLinkSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JLink").finish()
    }
}

impl ProbeDriver for JLinkSource {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        let mut jlinks = jaylink::scan_usb()?
            .filter(|usb_info| {
                usb_info.vendor_id() == selector.vendor_id
                    && usb_info.product_id() == selector.product_id
                    && selector
                        .serial_number
                        .as_ref()
                        .map(|s| usb_info.serial_number() == Some(s))
                        .unwrap_or(true)
            })
            .collect::<Vec<_>>();

        if jlinks.is_empty() {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                super::ProbeCreationError::NotFound,
            ));
        } else if jlinks.len() > 1 {
            tracing::warn!("More than one matching J-Link was found. Opening the first one.")
        }

        let jlink_handle = JayLink::open_usb(jlinks.pop().unwrap())?;

        // Check which protocols are supported by the J-Link.
        //
        // If the J-Link has the SELECT_IF capability, we can just ask
        // it which interfaces it supports. If it doesn't have the capabilty,
        // we assume that it just supports JTAG. In that case, we will also
        // not be able to change protocols.

        let supported_protocols: Vec<WireProtocol> =
            if jlink_handle.capabilities().contains(Capability::SelectIf) {
                let interfaces = jlink_handle.available_interfaces();

                let protocols: Vec<_> =
                    interfaces.into_iter().map(WireProtocol::try_from).collect();

                protocols
                    .iter()
                    .filter(|p| p.is_err())
                    .for_each(|protocol| {
                        if let Err(JlinkError::UnknownInterface(interface)) = protocol {
                            tracing::debug!(
                            "J-Link returned interface {:?}, which is not supported by probe-rs.",
                            interface
                        );
                        }
                    });

                // We ignore unknown protocols, the chance that this happens is pretty low,
                // and we can just work with the ones we know and support.
                protocols.into_iter().filter_map(Result::ok).collect()
            } else {
                // The J-Link cannot report which interfaces it supports, and cannot
                // switch interfaces. We assume it just supports JTAG.
                vec![WireProtocol::Jtag]
            };

        // Some devices can't handle large transfers, so we limit the chunk size
        // While it would be nice to read this directly from the device,
        // `read_max_mem_block`'s return value does not directly correspond to the
        // maximum transfer size, and it's not clear how to get the actual value.
        // The number of *bits* is encoded as a u16, so the maximum value is 65535
        let chunk_size = match selector.product_id {
            // 0x0101: J-Link EDU
            0x0101 => 65535,
            // 0x1051: J-Link OB-K22-SiFive: 504 bits
            0x1051 => 504,
            // Assume the lowest value is a safe default
            _ => 504,
        };

        Ok(Box::new(JLink {
            handle: jlink_handle,
            swo_config: None,
            supported_protocols,
            protocol: None,
            speed_khz: 0,
            swd_settings: SwdSettings::default(),
            probe_statistics: ProbeStatistics::default(),
            jtag_state: JtagDriverState::default(),

            jtag_tms_bits: vec![],
            jtag_tdi_bits: vec![],
            jtag_capture_tdo: vec![],
            jtag_response: BitVec::new(),

            chunk_size,
        }))
    }

    fn list_probes(&self) -> Vec<DebugProbeInfo> {
        list_jlink_devices()
    }
}

#[derive(Debug)]
pub(crate) struct JLink {
    handle: JayLink,
    swo_config: Option<SwoConfig>,

    /// Currently selected protocol
    protocol: Option<WireProtocol>,

    /// Protocols supported by the connected J-Link probe.
    supported_protocols: Vec<WireProtocol>,

    speed_khz: u32,

    jtag_tms_bits: Vec<bool>,
    jtag_tdi_bits: Vec<bool>,
    jtag_capture_tdo: Vec<bool>,
    jtag_response: BitVec<u8, Lsb0>,
    jtag_state: JtagDriverState,

    // max number of bits in a transfer chunk
    chunk_size: usize,

    probe_statistics: ProbeStatistics,
    swd_settings: SwdSettings,
}

impl JLink {
    fn select_interface(
        &mut self,
        protocol: Option<WireProtocol>,
    ) -> Result<WireProtocol, DebugProbeError> {
        let capabilities = self.handle.capabilities();

        if capabilities.contains(Capability::SelectIf) {
            if let Some(protocol) = protocol {
                let jlink_interface = match protocol {
                    WireProtocol::Swd => jaylink::Interface::Swd,
                    WireProtocol::Jtag => jaylink::Interface::Jtag,
                };

                if self.handle.available_interfaces().contains(jlink_interface) {
                    // We can select the desired interface
                    self.handle.select_interface(jlink_interface)?;
                    Ok(protocol)
                } else {
                    Err(DebugProbeError::UnsupportedProtocol(protocol))
                }
            } else {
                // No special protocol request
                let current_protocol = self.handle.current_interface();

                match current_protocol {
                    jaylink::Interface::Swd => Ok(WireProtocol::Swd),
                    jaylink::Interface::Jtag => Ok(WireProtocol::Jtag),
                    x => unimplemented!("J-Link: Protocol {} is not yet supported.", x),
                }
            }
        } else {
            // Assume JTAG protocol if the probe does not support switching interfaces
            match protocol {
                Some(WireProtocol::Jtag) => Ok(WireProtocol::Jtag),
                Some(p) => Err(DebugProbeError::UnsupportedProtocol(p)),
                None => Ok(WireProtocol::Jtag),
            }
        }
    }

    fn shift_bit(&mut self, tms: bool, tdi: bool, capture: bool) -> Result<(), DebugProbeError> {
        self.jtag_state.state.update(tms);

        self.jtag_tms_bits.push(tms);
        self.jtag_tdi_bits.push(tdi);
        self.jtag_capture_tdo.push(capture);

        if self.jtag_tms_bits.len() >= self.chunk_size {
            self.flush()?;
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<(), DebugProbeError> {
        if self.jtag_tms_bits.is_empty() {
            return Ok(());
        }

        let response = self.handle.jtag_io(
            std::mem::take(&mut self.jtag_tms_bits),
            std::mem::take(&mut self.jtag_tdi_bits),
        )?;

        for (bit, capture) in response.zip(std::mem::take(&mut self.jtag_capture_tdo)) {
            if capture {
                self.jtag_response.push(bit);
            }
        }

        Ok(())
    }

    fn read_captured_bits(&mut self) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        self.flush()?;

        Ok(std::mem::take(&mut self.jtag_response))
    }
}

impl DebugProbe for JLink {
    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        // try to select the interface

        let actual_protocol = self.select_interface(Some(protocol))?;

        if actual_protocol == protocol {
            self.protocol = Some(protocol);
            Ok(())
        } else {
            self.protocol = Some(actual_protocol);
            Err(DebugProbeError::UnsupportedProtocol(protocol))
        }
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        self.protocol
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

        if let Ok(speeds) = self.handle.read_speeds() {
            tracing::debug!("Supported speeds: {:?}", speeds);

            let max_speed_khz = speeds.max_speed_hz() / 1000;

            if max_speed_khz < speed_khz {
                return Err(DebugProbeError::UnsupportedSpeed(speed_khz));
            }
        };

        if let Some(expected_speed) = SpeedConfig::khz(speed_khz as u16) {
            self.handle.set_speed(expected_speed)?;
            self.speed_khz = speed_khz;
        } else {
            return Err(DebugProbeError::UnsupportedSpeed(speed_khz));
        }

        Ok(speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Attaching to J-Link");

        let configured_protocol = match self.protocol {
            Some(protocol) => protocol,
            None => {
                if self.supported_protocols.contains(&WireProtocol::Swd) {
                    WireProtocol::Swd
                } else {
                    // At least one protocol is always supported
                    *self.supported_protocols.first().unwrap()
                }
            }
        };

        let actual_protocol = self.select_interface(Some(configured_protocol))?;

        if actual_protocol != configured_protocol {
            tracing::warn!("Protocol {} is configured, but not supported by the probe. Using protocol {} instead", configured_protocol, actual_protocol);
        }

        tracing::debug!("Attaching with protocol '{}'", actual_protocol);
        self.protocol = Some(actual_protocol);

        // Get reference to JayLink instance
        let capabilities = self.handle.capabilities();

        // Log some information about the probe
        tracing::debug!("J-Link: Capabilities: {:?}", capabilities);
        let fw_version = self
            .handle
            .read_firmware_version()
            .unwrap_or_else(|_| "?".into());
        tracing::info!("J-Link: Firmware version: {}", fw_version);
        match self.handle.read_hardware_version() {
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

        match actual_protocol {
            WireProtocol::Jtag => {
                // try some JTAG stuff

                tracing::debug!("Resetting JTAG chain using trst");
                self.handle.reset_trst()?;

                let chain = self.scan_chain()?;
                tracing::info!("Found {} TAPs on reset scan", chain.len());

                if chain.len() > 1 {
                    tracing::warn!("More than one TAP detected, defaulting to tap0");
                }

                self.select_target(&chain, 0)?;
            }
            WireProtocol::Swd => {
                // Attaching is handled in sequence

                // We are ready to debug.
            }
        }

        tracing::debug!("Attached succesfully");

        Ok(())
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::NotImplemented("target_reset"))
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.handle.set_reset(false)?;
        Ok(())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.handle.set_reset(true)?;
        Ok(())
    }

    fn try_get_riscv_interface(
        mut self: Box<Self>,
    ) -> Result<RiscvCommunicationInterface, (Box<dyn DebugProbe>, RiscvError)> {
        if self.supported_protocols.contains(&WireProtocol::Jtag) {
            if let Err(e) = self.select_protocol(WireProtocol::Jtag) {
                return Err((self, e.into()));
            }
            match RiscvCommunicationInterface::new(self) {
                Ok(interface) => Ok(interface),
                Err((probe, err)) => Err((probe.into_probe(), err)),
            }
        } else {
            Err((self, DebugProbeError::InterfaceNotAvailable("JTAG").into()))
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
        Ok(Some((self.handle.read_target_voltage()? as f32) / 1000f32))
    }

    fn try_get_xtensa_interface(
        mut self: Box<Self>,
    ) -> Result<XtensaCommunicationInterface, (Box<dyn DebugProbe>, DebugProbeError)> {
        if self.supported_protocols.contains(&WireProtocol::Jtag) {
            if let Err(e) = self.select_protocol(WireProtocol::Jtag) {
                return Err((self, e));
            }
            match XtensaCommunicationInterface::new(self) {
                Ok(interface) => Ok(interface),
                Err((probe, err)) => Err((probe.into_probe(), err)),
            }
        } else {
            Err((self, DebugProbeError::InterfaceNotAvailable("JTAG")))
        }
    }

    fn has_xtensa_interface(&self) -> bool {
        self.supported_protocols.contains(&WireProtocol::Jtag)
    }
}

impl RawProtocolIo for JLink {
    fn jtag_shift_tms<M>(&mut self, tms: M, tdi: bool) -> Result<(), DebugProbeError>
    where
        M: IntoIterator<Item = bool>,
    {
        if self.protocol.unwrap() == crate::WireProtocol::Swd {
            panic!("Logic error, requested jtag_io when in SWD mode");
        }

        self.probe_statistics.report_io();

        self.shift_bits(tms, iter::repeat(tdi), iter::repeat(false))?;

        Ok(())
    }

    fn jtag_shift_tdi<I>(&mut self, tms: bool, tdi: I) -> Result<(), DebugProbeError>
    where
        I: IntoIterator<Item = bool>,
    {
        if self.protocol.unwrap() == crate::WireProtocol::Swd {
            panic!("Logic error, requested jtag_io when in SWD mode");
        }

        self.probe_statistics.report_io();

        self.shift_bits(iter::repeat(tms), tdi, iter::repeat(false))?;

        Ok(())
    }

    fn swd_io<D, S>(&mut self, dir: D, swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        D: IntoIterator<Item = bool>,
        S: IntoIterator<Item = bool>,
    {
        if self.protocol.unwrap() == crate::WireProtocol::Jtag {
            panic!("Logic error, requested swd_io when in JTAG mode");
        }

        self.probe_statistics.report_io();

        let iter = self.handle.swd_io(dir, swdio)?;

        Ok(iter.collect())
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
        self.shift_bit(tms, tdi, capture)
    }

    fn flush(&mut self) -> Result<(), DebugProbeError> {
        self.flush()
    }

    fn read_captured_bits(&mut self) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        self.read_captured_bits()
    }
}

impl DapProbe for JLink {}

impl SwoAccess for JLink {
    fn enable_swo(&mut self, config: &SwoConfig) -> Result<(), ArmError> {
        self.swo_config = Some(*config);
        self.handle
            .swo_start(SwoMode::Uart, config.baud(), SWO_BUFFER_SIZE.into())
            .map_err(|e| ArmError::from(DebugProbeError::ProbeSpecific(Box::new(e))))?;
        Ok(())
    }

    fn disable_swo(&mut self) -> Result<(), ArmError> {
        self.swo_config = None;
        self.handle
            .swo_stop()
            .map_err(|e| ArmError::from(DebugProbeError::ProbeSpecific(Box::new(e))))?;
        Ok(())
    }

    fn swo_buffer_size(&mut self) -> Option<usize> {
        Some(SWO_BUFFER_SIZE.into())
    }

    fn read_swo_timeout(&mut self, timeout: std::time::Duration) -> Result<Vec<u8>, ArmError> {
        let end = std::time::Instant::now() + timeout;
        let mut buf = vec![0; SWO_BUFFER_SIZE.into()];

        let poll_interval = self
            .swo_poll_interval_hint(&self.swo_config.unwrap())
            .unwrap();

        let mut bytes = vec![];
        loop {
            let data = self
                .handle
                .swo_read(&mut buf)
                .map_err(|e| ArmError::from(DebugProbeError::ProbeSpecific(Box::new(e))))?;
            bytes.extend(data.as_ref());
            let now = std::time::Instant::now();
            if now + poll_interval < end {
                std::thread::sleep(poll_interval);
            } else {
                break;
            }
        }
        Ok(bytes)
    }
}

#[tracing::instrument(skip_all)]
fn list_jlink_devices() -> Vec<DebugProbeInfo> {
    match jaylink::scan_usb() {
        Ok(devices) => devices
            .map(|device_info| {
                DebugProbeInfo::new(
                    format!(
                        "J-Link{}",
                        device_info
                            .product_string()
                            .map(|p| format!(" ({p})"))
                            .unwrap_or_default()
                    ),
                    device_info.vendor_id(),
                    device_info.product_id(),
                    device_info.serial_number().map(|s| s.to_string()),
                    &JLinkSource,
                    None,
                )
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

impl From<jaylink::Error> for DebugProbeError {
    fn from(e: jaylink::Error) -> DebugProbeError {
        DebugProbeError::ProbeSpecific(Box::new(e))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JlinkError {
    #[error("Unknown interface reported by J-Link: {0:?}")]
    UnknownInterface(jaylink::Interface),
}

impl TryFrom<jaylink::Interface> for WireProtocol {
    type Error = JlinkError;

    fn try_from(interface: Interface) -> Result<Self, Self::Error> {
        match interface {
            Interface::Jtag => Ok(WireProtocol::Jtag),
            Interface::Swd => Ok(WireProtocol::Swd),
            unknown_interface => Err(JlinkError::UnknownInterface(unknown_interface)),
        }
    }
}
