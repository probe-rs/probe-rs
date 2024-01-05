//! Support for J-Link Debug probes

mod jaylink;

use anyhow::anyhow;

use bitvec::prelude::*;
use bitvec::vec::BitVec;
use jaylink::{Capability, Interface, JayLink, SpeedConfig, SwoMode};
use probe_rs_target::ScanChainElement;

use std::convert::TryFrom;
use std::iter;

use crate::architecture::arm::ArmError;
use crate::architecture::riscv::communication_interface::RiscvError;
use crate::architecture::xtensa::communication_interface::XtensaCommunicationInterface;
use crate::probe::common::{
    common_sequence, extract_idcodes, extract_ir_lengths, JtagState, RegisterState,
};
use crate::probe::ProbeDriver;
use crate::probe::{ChainParams, JtagChainItem};
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
        DebugProbe, DebugProbeError, DebugProbeInfo, JTAGAccess, WireProtocol,
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
        // we assume that it justs support JTAG. In that case, we will also
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

        Ok(Box::new(JLink {
            handle: jlink_handle,
            swo_config: None,
            supported_protocols,
            jtag_idle_cycles: 0,
            protocol: None,
            current_ir_reg: 1,
            speed_khz: 0,
            scan_chain: None,
            swd_settings: SwdSettings::default(),
            probe_statistics: ProbeStatistics::default(),
            max_ir_address: 0x1F,
            chain_params: ChainParams::default(),
            jtag_state: JtagState::Reset,
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

    /// Idle cycles necessary between consecutive
    /// accesses to the DMI register
    jtag_idle_cycles: u8,

    /// Currently selected protocol
    protocol: Option<WireProtocol>,

    /// Protocols supported by the connected J-Link probe.
    supported_protocols: Vec<WireProtocol>,

    current_ir_reg: u32,
    max_ir_address: u32,

    speed_khz: u32,

    scan_chain: Option<Vec<ScanChainElement>>,
    chain_params: ChainParams,

    jtag_state: JtagState,

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

    fn read_dr(&mut self, register_bits: usize) -> Result<Vec<u8>, DebugProbeError> {
        tracing::debug!("Read {} bits from DR", register_bits);

        self.write_dr(&vec![0x00; (register_bits + 7) / 8], register_bits)
    }

    /// Write IR register with the specified data. The
    /// IR register might have an odd length, so the dta
    /// will be truncated to `len` bits. If data has less
    /// than `len` bits, an error will be returned.
    fn write_ir(&mut self, data: &[u8], len: usize) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        tracing::debug!("Write IR: {:?}, len={}", data, len);

        // Check the bit length, enough data has to be available
        if data.len() * 8 < len || len == 0 {
            return Err(DebugProbeError::Other(anyhow!("Invalid data length")));
        }

        // BYPASS commands before and after shifting out data where required
        let pre_bits = self.chain_params.irpre;
        let post_bits = self.chain_params.irpost;

        // The last bit will be transmitted when exiting the shift state,
        // so we need to stay in the shift state for one period less than
        // we have bits to transmit.
        let tms_data = iter::repeat(false).take(len - 1);

        // Enter IR shift
        self.jtag_move_to_state(JtagState::Ir(RegisterState::Shift))?;

        let tms = iter::repeat(false)
            .take(pre_bits)
            .chain(tms_data)
            .chain(iter::repeat(false).take(post_bits))
            .chain(iter::once(true));

        let tdi = iter::repeat(true)
            .take(pre_bits)
            .chain(data.as_bits::<Lsb0>()[..len].iter().map(|b| *b))
            .chain(iter::repeat(true).take(post_bits));

        tracing::trace!("tms: {:?}", tms.clone());
        tracing::trace!("tdi: {:?}", tdi.clone());

        let response = self.jtag_io(tms, tdi)?;

        let result = response[pre_bits..][..len].to_bitvec();

        self.jtag_move_to_state(JtagState::Ir(RegisterState::Update))?;

        Ok(result)
    }

    fn write_dr(&mut self, data: &[u8], register_bits: usize) -> Result<Vec<u8>, DebugProbeError> {
        tracing::debug!("Write DR: {:?}, len={}", data, register_bits);

        // Check the bit length, enough data has to be available
        if data.len() * 8 < register_bits || register_bits == 0 {
            return Err(DebugProbeError::Other(anyhow!("Invalid data length")));
        }

        // Last bit of data is shifted out when we exit the SHIFT-DR State
        let tms_shift_out_value = iter::repeat(false).take(register_bits - 1);

        // Enter DR shift
        self.jtag_move_to_state(JtagState::Dr(RegisterState::Shift))?;

        // dummy bits to account for bypasses
        let pre_bits = self.chain_params.drpre;
        let post_bits = self.chain_params.drpost;

        let tms = iter::repeat(false)
            .take(pre_bits)
            .chain(tms_shift_out_value)
            .chain(iter::repeat(false).take(post_bits))
            .chain(iter::once(true));

        let tdi = iter::repeat(false)
            .take(pre_bits)
            .chain(data.as_bits::<Lsb0>()[..register_bits].iter().map(|b| *b))
            .chain(iter::repeat(false).take(post_bits));

        let response = self.jtag_io(tms, tdi)?;

        self.jtag_move_to_state(JtagState::Dr(RegisterState::Update))?;

        if self.idle_cycles() > 0 {
            self.jtag_move_to_state(JtagState::Idle)?;

            // We need to stay in the idle cycle a bit
            let tms = iter::repeat(false).take(self.idle_cycles() as usize);
            let tdi = iter::repeat(false).take(self.idle_cycles() as usize);

            self.jtag_io(tms, tdi)?;
        }

        tracing::trace!("Response: {:?}", response);

        let mut result = response[pre_bits..][..register_bits].to_bitvec();

        tracing::trace!("result: {:?}", result);

        result.force_align();
        Ok(result.into_vec())
    }

    fn jtag_move_to_state(&mut self, target: JtagState) -> Result<(), DebugProbeError> {
        tracing::debug!("Changing state: {:?} -> {:?}", self.jtag_state, target);
        let mut steps = vec![];
        while let Some(tms) = self.jtag_state.step_toward(target) {
            steps.push(tms);
            self.jtag_state.update(tms);
        }
        let tdi = std::iter::repeat(false).take(steps.len());
        // Don't use jtag_io here, as we don't want to update the state twice
        self.handle.jtag_io(steps, tdi)?;
        tracing::debug!("In state: {:?}", self.jtag_state);
        Ok(())
    }

    fn jtag_io(
        &mut self,
        tms: impl IntoIterator<Item = bool> + Clone,
        tdi: impl IntoIterator<Item = bool>,
    ) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        let tms_bits = tms.clone().into_iter();
        let tdi_bits = tdi.into_iter();

        let response = self.handle.jtag_io(tms_bits, tdi_bits)?;
        let response = BitVec::<u8, Lsb0>::from_iter(response);

        for tms in tms.into_iter() {
            self.jtag_state.update(tms);
        }

        Ok(response)
    }

    fn jtag_reset(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Resetting JTAG chain by setting tms high for 5 bits");

        // Reset JTAG chain (5 times TMS high), and enter idle state afterwards
        let tms = [true, true, true, true, true, false];
        let tdi = iter::repeat(false).take(6);

        let response = self.jtag_io(tms, tdi)?;

        tracing::debug!("Response to reset: {:?}", response);

        Ok(())
    }

    fn scan(&mut self) -> Result<Vec<JtagChainItem>, DebugProbeError> {
        let max_chain = 8;

        self.jtag_reset()?;

        let input = vec![0xFF; 4 * max_chain];
        let response = self.write_dr(&input, input.len() * 8).unwrap();

        tracing::trace!("DR: {:?}", response);

        let idcodes = extract_idcodes(BitSlice::<u8, Lsb0>::from_slice(&response))
            .map_err(|e| DebugProbeError::Other(e.into()))?;

        tracing::info!(
            "JTAG DR scan complete, found {} TAPs. {:?}",
            idcodes.len(),
            idcodes
        );

        // First shift out all ones
        let input = vec![0xff; idcodes.len()];
        let response = self.write_ir(&input, input.len() * 8).unwrap();

        // Next, shift out same amount of zeros, then ones to make sure the IRs contain BYPASS.
        let input = iter::repeat(0)
            .take(idcodes.len())
            .chain(input)
            .collect::<Vec<_>>();
        let response_zeros = self.write_ir(&input, input.len() * 8).unwrap();

        let expected = if let Some(ref chain) = self.scan_chain {
            let expected = chain
                .iter()
                .filter_map(|s| s.ir_len)
                .map(|s| s as usize)
                .collect::<Vec<usize>>();
            Some(expected)
        } else {
            None
        };

        let response = response.as_bitslice();
        let response = common_sequence(response, response_zeros.as_bitslice());

        tracing::debug!("IR scan: {}", response);

        let ir_lens = extract_ir_lengths(response, idcodes.len(), expected.as_deref()).unwrap();
        tracing::debug!("Detected IR lens: {:?}", ir_lens);

        Ok(idcodes
            .into_iter()
            .zip(ir_lens)
            .map(|(idcode, irlen)| JtagChainItem { irlen, idcode })
            .collect())
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
        self.scan_chain = Some(scan_chain);
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

                let taps = self.scan()?;
                tracing::info!("Found {} TAPs on reset scan", taps.len());

                let selected = 0;
                if taps.len() > 1 {
                    tracing::warn!("More than one TAP detected, defaulting to tap0");
                }

                let Some(params) = ChainParams::from_jtag_chain(&taps, selected) else {
                    return Err(DebugProbeError::TargetNotFound);
                };

                tracing::info!("Setting chain params: {:?}", params);

                // set the max address to the max number of bits irlen can represent
                self.max_ir_address = (1 << params.irlen) - 1;
                tracing::debug!("Setting max_ir_address to {}", self.max_ir_address);
                self.chain_params = params;
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
            Err((self, DebugProbeError::InterfaceNotAvailable("JTAG").into()))
        }
    }

    fn has_xtensa_interface(&self) -> bool {
        self.supported_protocols.contains(&WireProtocol::Jtag)
    }
}

impl JTAGAccess for JLink {
    fn set_ir_len(&mut self, len: u32) {
        self.chain_params.irlen = len as usize;
    }

    /// Read the data register
    fn read_register(&mut self, address: u32, len: u32) -> Result<Vec<u8>, DebugProbeError> {
        let address_bits = address.to_le_bytes();

        if address > self.max_ir_address {
            return Err(DebugProbeError::Other(anyhow!(
                "JTAG Register addresses are fixed to {} bits",
                self.chain_params.irlen
            )));
        }

        if self.current_ir_reg != address {
            // Write IR register
            self.write_ir(&address_bits, self.chain_params.irlen)?;
            self.current_ir_reg = address;
        }

        // read DR register
        self.read_dr(len as usize)
    }

    /// Write the data register
    fn write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
    ) -> Result<Vec<u8>, DebugProbeError> {
        let address_bits = address.to_le_bytes();

        if address > self.max_ir_address {
            return Err(DebugProbeError::Other(anyhow!(
                "JTAG Register addresses are fixed to {} bits",
                self.chain_params.irlen
            )));
        }

        if self.current_ir_reg != address {
            // Write IR register
            self.write_ir(&address_bits, self.chain_params.irlen)?;
            self.current_ir_reg = address;
        }

        // write DR register
        self.write_dr(data, len as usize)
    }

    fn set_idle_cycles(&mut self, idle_cycles: u8) {
        self.jtag_idle_cycles = idle_cycles;
    }

    fn idle_cycles(&self) -> u8 {
        self.jtag_idle_cycles
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
        let tms_iter: Vec<_> = tms.into_iter().collect();
        let count = tms_iter.len();

        self.handle
            .jtag_io(tms_iter, std::iter::repeat(tdi).take(count))?;
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
        let tdi_iter: Vec<_> = tdi.into_iter().collect();
        let count = tdi_iter.len();

        self.handle
            .jtag_io(std::iter::repeat(tms).take(count), tdi_iter)?;
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
