use crate::architecture::arm::{
    ArmProbeInterface, DapAccess, FullyQualifiedApAddress, RawDapAccess, SwoAccess,
    ap::{
        self, AccessPortType, AddressIncrement, CSW, DataSize,
        memory_ap::{MemoryAp, MemoryApType},
        v1::valid_access_ports,
    },
    communication_interface::{DapProbe, DpState, SelectCache, SwdSequence, UninitializedArmProbe},
    dp::{
        Ctrl, DPIDR, DebugPortError, DebugPortId, DebugPortVersion, DpAccess, DpAddress,
        DpRegisterAddress, Select1, SelectV3,
    },
    memory::ArmMemoryInterface,
    sequences::ArmDebugSequence,
};
use crate::probe::blackmagic::{Align, BlackMagicProbe, ProtocolVersion, RemoteCommand};
use crate::probe::{ArmError, DebugProbeError, Probe};
use crate::{Error as ProbeRsError, MemoryInterface};
use std::collections::BTreeSet;
use std::collections::hash_map;
use std::{collections::HashMap, sync::Arc};
use zerocopy::IntoBytes;

#[derive(Debug)]
pub(crate) struct UninitializedBlackMagicArmProbe {
    probe: Box<BlackMagicProbe>,
}

#[derive(Debug)]
pub(crate) struct BlackMagicProbeArmDebug {
    probe: Box<BlackMagicProbe>,

    /// Information about the APs of the target.
    /// APs are identified by a number, starting from zero.
    pub access_ports: BTreeSet<FullyQualifiedApAddress>,

    /// A copy of the sequence that was passed during initialization
    sequence: Arc<dyn ArmDebugSequence>,

    /// The currently selected Debug Port. Used for multi-drop targets.
    current_dp: DpAddress,

    /// A list of all discovered Debug Ports.
    dps: HashMap<DpAddress, DpState>,

    /// Whether to enable a hardware feature to detect overruns
    use_overrun_detect: bool,
}

#[derive(Debug)]
pub(crate) struct BlackMagicProbeMemoryInterface<'probe> {
    probe: &'probe mut BlackMagicProbeArmDebug,
    current_ap: MemoryAp,
    index: u8,
    apsel: u8,
    csw: u32,
}

impl UninitializedBlackMagicArmProbe {
    pub fn new(probe: Box<BlackMagicProbe>) -> Self {
        Self { probe }
    }
}

impl UninitializedArmProbe for UninitializedBlackMagicArmProbe {
    fn initialize(
        mut self: Box<Self>,
        sequence: Arc<dyn ArmDebugSequence>,
        dp: DpAddress,
    ) -> Result<Box<dyn ArmProbeInterface>, (Box<dyn UninitializedArmProbe>, ProbeRsError)> {
        // Switch to the correct mode
        if let Err(err) = tracing::debug_span!("debug_port_setup")
            .in_scope(|| sequence.debug_port_setup(&mut *self.probe, dp))
        {
            return Err((self, err.into()));
        }

        let interface = BlackMagicProbeArmDebug::new(self.probe, dp, sequence)
            .map_err(|(s, e)| (s as Box<_>, ProbeRsError::from(e)))?;

        Ok(Box::new(interface))
    }

    fn close(self: Box<Self>) -> Probe {
        Probe::from_attached_probe(self.probe)
    }
}

impl SwdSequence for UninitializedBlackMagicArmProbe {
    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        self.probe.swj_sequence(bit_len, bits)
    }

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        self.probe.swj_pins(pin_out, pin_select, pin_wait)
    }
}

impl BlackMagicProbeArmDebug {
    fn new(
        probe: Box<BlackMagicProbe>,
        dp: DpAddress,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Self, (Box<UninitializedBlackMagicArmProbe>, ArmError)> {
        let mut interface = Self {
            probe,
            access_ports: BTreeSet::new(),
            sequence,
            current_dp: dp,
            dps: HashMap::new(),
            use_overrun_detect: true,
        };

        if let Err(e) = interface.select_dp(dp) {
            return Err((
                Box::new(UninitializedBlackMagicArmProbe {
                    probe: interface.probe,
                }),
                e,
            ));
        }

        interface.access_ports = valid_access_ports(&mut interface, dp)
            .into_iter()
            .inspect(|addr| tracing::debug!("AP {:#x?}", addr))
            .collect();
        Ok(interface)
    }

    /// Connect to the target debug port and power it up. This is based on the
    /// `DebugPortStart` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.html#debugPortStart
    fn debug_port_start(&mut self, dp: DpAddress) -> Result<(), ArmError> {
        self.sequence.clone().debug_port_start(self, dp)
    }

    fn select_dp(&mut self, dp: DpAddress) -> Result<&mut DpState, ArmError> {
        let mut switched_dp = false;

        let sequence = self.sequence.clone();

        if self.current_dp != dp {
            tracing::debug!("Selecting DP {:x?}", dp);

            switched_dp = true;

            self.probe.raw_flush()?;

            // Try to switch to the new DP.
            if let Err(e) = sequence.debug_port_connect(&mut *self.probe, dp) {
                tracing::warn!("Failed to switch to DP {:x?}: {}", dp, e);

                // Try the more involved debug_port_setup sequence, which also handles dormant mode.
                sequence.debug_port_setup(&mut *self.probe, dp)?;
            }

            self.current_dp = dp;
        }

        // If we don't have  a state for this DP, this means that we haven't run the necessary init sequence yet.
        if let hash_map::Entry::Vacant(entry) = self.dps.entry(dp) {
            let sequence = self.sequence.clone();

            entry.insert(DpState::new());

            let start_span = tracing::debug_span!("debug_port_start").entered();
            sequence.debug_port_start(self, dp)?;
            drop(start_span);

            // Make sure we enable the overrun detect mode when requested.
            // For "bit-banging" probes, such as JLink or FTDI, we rely on it for good, stable communication.
            // This is required as the default sequence (and most special implementations) does not do this.
            let mut ctrl_reg: Ctrl = self.read_dp_register(dp)?;
            if ctrl_reg.orun_detect() != self.use_overrun_detect {
                tracing::debug!("Setting orun_detect: {}", self.use_overrun_detect);
                // only write if there’s a need for it.
                ctrl_reg.set_orun_detect(self.use_overrun_detect);
                self.write_dp_register(dp, ctrl_reg)?;
            }

            let idr: DebugPortId = self.read_dp_register::<DPIDR>(dp)?.into();
            tracing::info!(
                "Debug Port version: {} MinDP: {:?}",
                idr.version,
                idr.min_dp_support
            );

            let state = self
                .dps
                .get_mut(&dp)
                .expect("This DP State was inserted earlier in this function");
            state.debug_port_version = idr.version;
            if idr.version == DebugPortVersion::DPv3 {
                state.current_select = SelectCache::DPv3(SelectV3(0), Select1(0));
            }
        } else if switched_dp {
            let sequence = self.sequence.clone();

            let start_span = tracing::debug_span!("debug_port_start").entered();
            sequence.debug_port_start(self, dp)?;
            drop(start_span);
        }

        // note(unwrap): Entry gets inserted above
        Ok(self.dps.get_mut(&dp).unwrap())
    }

    fn select_dp_and_dp_bank(
        &mut self,
        dp: DpAddress,
        dp_register_address: &DpRegisterAddress,
    ) -> Result<(), ArmError> {
        let dp_state = self.select_dp(dp)?;

        // DP register addresses are 4 bank bits, 4 address bits. Lowest 2 address bits are
        // always 0, so this leaves only 4 possible addresses: 0x0, 0x4, 0x8, 0xC.
        // On ADIv5, only address 0x4 is banked, the rest are don't care.
        // On ADIv6, address 0x0 and 0x4 are banked, the rest are don't care.

        let &DpRegisterAddress {
            bank,
            address: addr,
        } = dp_register_address;

        if addr != 0 && addr != 4 {
            return Ok(());
        }

        let bank = bank.unwrap_or(0);

        if bank != dp_state.current_select.dp_bank_sel() {
            dp_state.current_select.set_dp_bank_sel(bank);

            tracing::debug!("Changing DP_BANK_SEL to {:x?}", dp_state.current_select);

            match dp_state.current_select {
                SelectCache::DPv1(select) => self.write_dp_register(dp, select)?,
                SelectCache::DPv3(select, _) => self.write_dp_register(dp, select)?,
            }
        }

        Ok(())
    }

    fn select_ap(&mut self, ap: &FullyQualifiedApAddress) -> Result<u8, ArmError> {
        let apsel = match ap.ap() {
            crate::architecture::arm::ApAddress::V1(val) => *val,
            crate::architecture::arm::ApAddress::V2(_) => {
                return Err(ArmError::NotImplemented(
                    "AP address v2 currently unsupported",
                ));
            }
        };
        self.select_dp(ap.dp())?;
        Ok(apsel)
    }
}

impl ArmProbeInterface for BlackMagicProbeArmDebug {
    fn access_ports(
        &mut self,
        dp: DpAddress,
    ) -> Result<BTreeSet<FullyQualifiedApAddress>, ArmError> {
        let state = self.select_dp(dp)?;
        match state.debug_port_version {
            DebugPortVersion::DPv0 | DebugPortVersion::DPv1 | DebugPortVersion::DPv2 => {
                Ok(ap::v1::valid_access_ports(self, dp).into_iter().collect())
            }
            DebugPortVersion::DPv3 => ap::v2::enumerate_access_ports(self, dp),
            DebugPortVersion::Unsupported(_) => unreachable!(),
        }
    }

    fn close(self: Box<Self>) -> Probe {
        Probe::from_attached_probe(self.probe)
    }

    fn current_debug_port(&self) -> DpAddress {
        DpAddress::Default
    }

    fn memory_interface(
        &mut self,
        access_port: &FullyQualifiedApAddress,
    ) -> Result<Box<dyn crate::architecture::arm::memory::ArmMemoryInterface + '_>, ArmError> {
        let mut current_ap = MemoryAp::new(self, access_port)?;

        // Construct a CSW to pass to the AP when accessing memory.
        let csw: CSW = match &mut current_ap {
            MemoryAp::AmbaAhb3(ap) => {
                let mut csw = ap.status(self)?;

                csw.DbgSwEnable = true;
                csw.AddrInc = AddressIncrement::Off;
                csw.Size = DataSize::U8;

                csw.MasterType = true;
                csw.Privileged = true;
                csw.Data = true;

                csw.Allocate = false;
                csw.Cacheable = false;
                csw.Bufferable = false;

                // Enable secure access if it's allowed
                csw.HNONSEC = !csw.SPIDEN;

                CSW::try_from(Into::<u32>::into(csw))?
            }
            MemoryAp::AmbaAhb5(ap) => {
                let mut csw = ap.status(self)?;

                csw.DbgSwEnable = true;
                csw.AddrInc = AddressIncrement::Off;
                csw.Size = DataSize::U8;

                csw.MasterType = true;
                csw.Data = true;
                csw.Privileged = true;

                // Enable secure access if it's allowed
                csw.HNONSEC = !csw.SPIDEN;

                CSW::try_from(Into::<u32>::into(csw))?
            }
            MemoryAp::AmbaAhb5Hprot(ap) => {
                let mut csw = ap.status(self)?;

                csw.DbgSwEnable = true;
                csw.AddrInc = AddressIncrement::Off;
                csw.Size = DataSize::U8;

                csw.MasterType = true;
                csw.Data = true;
                csw.Privileged = true;

                // Enable secure access if it's allowed
                csw.HNONSEC = !csw.SPIDEN;

                CSW::try_from(Into::<u32>::into(csw))?
            }
            MemoryAp::AmbaApb2Apb3(ap) => {
                let mut csw = ap.status(self)?;

                csw.DbgSwEnable = true;
                csw.AddrInc = AddressIncrement::Off;
                csw.Size = DataSize::U8;

                CSW::try_from(Into::<u32>::into(csw))?
            }
            MemoryAp::AmbaApb4Apb5(ap) => {
                let mut csw = ap.status(self)?;

                csw.DbgSwEnable = true;
                csw.AddrInc = AddressIncrement::Off;
                csw.Size = DataSize::U8;

                // Enable secure access if it's allowed
                csw.NonSecure = !csw.SPIDEN;
                csw.Privileged = true;

                CSW::try_from(Into::<u32>::into(csw))?
            }
            MemoryAp::AmbaAxi3Axi4(ap) => {
                let mut csw = ap.status(self)?;

                csw.DbgSwEnable = true;
                csw.AddrInc = AddressIncrement::Off;
                csw.Size = DataSize::U8;

                csw.Instruction = false;
                // Enable secure access if it's allowed
                csw.NonSecure = !csw.SPIDEN;
                csw.Privileged = true;
                csw.CACHE = 0;

                CSW::try_from(Into::<u32>::into(csw))?
            }
            MemoryAp::AmbaAxi5(ap) => {
                let mut csw = ap.status(self)?;

                csw.DbgSwEnable = true;
                csw.AddrInc = AddressIncrement::Off;
                csw.Size = DataSize::U8;

                csw.Instruction = false;
                // Enable secure access if it's allowed
                csw.NonSecure = !csw.SPIDEN;
                csw.Privileged = true;
                csw.CACHE = 0;
                csw.MTE = false;

                CSW::try_from(Into::<u32>::into(csw))?
            }
        };

        Ok(Box::new(BlackMagicProbeMemoryInterface {
            probe: self,
            current_ap,
            index: 0,
            apsel: 0,
            csw: csw.into(),
        }) as _)
    }

    fn reinitialize(&mut self) -> Result<(), ArmError> {
        let sequence = self.sequence.clone();
        let dp = self.current_debug_port();

        // Switch to the correct mode
        sequence.debug_port_setup(&mut *self.probe, dp)?;

        if let Err(e) = sequence.debug_port_connect(&mut *self.probe, dp) {
            tracing::warn!("failed to switch to DP {:x?}: {}", dp, e);

            // Try the more involved debug_port_setup sequence, which also handles dormant mode.
            sequence.debug_port_setup(&mut *self.probe, dp)?;
        }

        self.debug_port_start(dp)?;

        self.access_ports = valid_access_ports(self, DpAddress::Default)
            .into_iter()
            .inspect(|addr| tracing::debug!("AP {:#x?}", addr))
            .collect();

        Ok(())
    }
}

impl SwoAccess for BlackMagicProbeArmDebug {
    fn enable_swo(
        &mut self,
        _config: &crate::architecture::arm::SwoConfig,
    ) -> Result<(), ArmError> {
        Err(ArmError::NotImplemented("swo not implemented"))
    }

    fn disable_swo(&mut self) -> Result<(), ArmError> {
        Err(ArmError::NotImplemented("swo not implemented"))
    }

    fn read_swo_timeout(&mut self, _timeout: std::time::Duration) -> Result<Vec<u8>, ArmError> {
        Err(ArmError::NotImplemented("swo not implemented"))
    }
}

impl SwdSequence for BlackMagicProbeArmDebug {
    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        self.probe.swj_sequence(bit_len, bits)
    }

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        self.probe.swj_pins(pin_out, pin_select, pin_wait)
    }
}

impl DapAccess for BlackMagicProbeArmDebug {
    fn read_raw_dp_register(
        &mut self,
        dp: DpAddress,
        address: DpRegisterAddress,
    ) -> Result<u32, ArmError> {
        self.select_dp_and_dp_bank(dp, &address)?;
        let index = 0;
        let command = match self.probe.remote_protocol {
            ProtocolVersion::V0 => {
                return Err(ArmError::Probe(
                    DebugProbeError::CommandNotSupportedByProbe {
                        command_name: "adiv5 raw dp read",
                    },
                ));
            }
            ProtocolVersion::V0P => RemoteCommand::ReadDpV0P {
                addr: address.into(),
            },
            ProtocolVersion::V1 | ProtocolVersion::V2 => RemoteCommand::ReadDpV1 {
                index,
                addr: address.into(),
            },
            ProtocolVersion::V3 | ProtocolVersion::V4 => RemoteCommand::ReadDpV3 {
                index,
                addr: address.into(),
            },
        };
        Ok(u32::from_be(
            TryInto::<u32>::try_into(
                self.probe
                    .command(command)
                    .map_err(|e| ArmError::Probe(e.into()))?
                    .0,
            )
            .unwrap(),
        ))
    }

    fn write_raw_dp_register(
        &mut self,
        dp: DpAddress,
        address: DpRegisterAddress,
        value: u32,
    ) -> Result<(), ArmError> {
        self.select_dp_and_dp_bank(dp, &address)?;
        let index = 0;
        let command = match self.probe.remote_protocol {
            ProtocolVersion::V0 => {
                return Err(ArmError::Probe(
                    DebugProbeError::CommandNotSupportedByProbe {
                        command_name: "adiv5 raw dp write",
                    },
                ));
            }
            ProtocolVersion::V0P => RemoteCommand::RawAccessV0P {
                rnw: 0,
                addr: address.into(),
                value,
            },
            ProtocolVersion::V1 | ProtocolVersion::V2 => RemoteCommand::RawAccessV1 {
                index,
                rnw: 0,
                addr: address.into(),
                value,
            },
            ProtocolVersion::V3 | ProtocolVersion::V4 => RemoteCommand::RawAccessV3 {
                index,
                rnw: 0,
                addr: address.into(),
                value,
            },
        };
        let result = self
            .probe
            .command(command)
            .map_err(|e| ArmError::Probe(e.into()))?
            .0;
        if result == 0 {
            Ok(())
        } else {
            Err(ArmError::Probe(DebugProbeError::Other(format!(
                "probe returned unexpected result: {}",
                result
            ))))
        }
    }

    fn read_raw_ap_register(
        &mut self,
        ap: &FullyQualifiedApAddress,
        addr: u64,
    ) -> Result<u32, ArmError> {
        // Currently, only APv1 is supported. As such, truncate the address to an 8-bit size.
        if ap.ap().is_v2() {
            return Err(ArmError::NotImplemented(
                "BlackMagicProbe does not yet support APv2",
            ));
        }
        let index = ((addr >> 8) & 0xFF) as u8;
        let addr = (addr & 0xFF) as u8;
        let apsel = self.select_ap(ap)?;

        let command = match self.probe.remote_protocol {
            ProtocolVersion::V0 => {
                return Err(ArmError::Probe(
                    DebugProbeError::CommandNotSupportedByProbe {
                        command_name: "adiv5 raw ap read",
                    },
                ));
            }
            ProtocolVersion::V0P => RemoteCommand::ReadApV0P { apsel, addr },
            ProtocolVersion::V1 | ProtocolVersion::V2 => {
                RemoteCommand::ReadApV1 { index, apsel, addr }
            }
            ProtocolVersion::V3 | ProtocolVersion::V4 => {
                RemoteCommand::ReadApV3 { index, apsel, addr }
            }
        };
        let result = u32::from_be(
            TryInto::<u32>::try_into(
                self.probe
                    .command(command)
                    .map_err(|e| ArmError::Probe(e.into()))?
                    .0,
            )
            .unwrap(),
        );
        Ok(result)
    }

    fn write_raw_ap_register(
        &mut self,
        ap: &FullyQualifiedApAddress,
        addr: u64,
        value: u32,
    ) -> Result<(), ArmError> {
        // Currently, only APv1 is supported. As such, truncate the address to an 8-bit size.
        if ap.ap().is_v2() {
            return Err(ArmError::NotImplemented(
                "BlackMagicProbe does not yet support APv2",
            ));
        }
        let index = ((addr >> 8) & 0xFF) as u8;
        let addr = (addr & 0xFF) as u8;

        let apsel = self.select_ap(ap)?;
        let command = match self.probe.remote_protocol {
            ProtocolVersion::V0 => {
                return Err(ArmError::Probe(
                    DebugProbeError::CommandNotSupportedByProbe {
                        command_name: "adiv5 raw ap write",
                    },
                ));
            }
            ProtocolVersion::V0P => RemoteCommand::WriteApV0P { apsel, addr, value },
            ProtocolVersion::V1 | ProtocolVersion::V2 => RemoteCommand::WriteApV1 {
                index,
                apsel,
                addr,
                value,
            },
            ProtocolVersion::V3 | ProtocolVersion::V4 => RemoteCommand::WriteApV3 {
                index,
                apsel,
                addr,
                value,
            },
        };

        let result = self
            .probe
            .command(command)
            .map_err(|e| ArmError::Probe(e.into()))?
            .0;
        if result == 0 {
            Ok(())
        } else {
            Err(ArmError::Probe(DebugProbeError::Other(format!(
                "probe returned unexpected result: {}",
                result
            ))))
        }
    }

    fn try_dap_probe(&self) -> Option<&dyn DapProbe> {
        Some(&*self.probe)
    }

    fn try_dap_probe_mut(&mut self) -> Option<&mut dyn DapProbe> {
        Some(&mut *self.probe)
    }
}

impl ArmMemoryInterface for BlackMagicProbeMemoryInterface<'_> {
    fn fully_qualified_address(&self) -> FullyQualifiedApAddress {
        self.current_ap.ap_address().clone()
    }

    fn base_address(&mut self) -> Result<u64, ArmError> {
        self.current_ap.base_address(self.probe)
    }

    fn get_swd_sequence(&mut self) -> Result<&mut dyn SwdSequence, DebugProbeError> {
        Ok(self.probe)
    }

    fn get_arm_probe_interface(&mut self) -> Result<&mut dyn ArmProbeInterface, DebugProbeError> {
        Ok(self.probe)
    }

    fn get_dap_access(&mut self) -> Result<&mut dyn DapAccess, DebugProbeError> {
        Ok(self.probe)
    }

    fn generic_status(&mut self) -> Result<crate::architecture::arm::ap::CSW, ArmError> {
        let csw = CSW::try_from(self.csw)
            .map_err(|e| ArmError::DebugPort(DebugPortError::RegisterParse(e)))?;

        Ok(csw)
    }
}

impl SwdSequence for BlackMagicProbeMemoryInterface<'_> {
    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        self.probe.swj_sequence(bit_len, bits)
    }

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        self.probe.swj_pins(pin_out, pin_select, pin_wait)
    }
}

impl BlackMagicProbeMemoryInterface<'_> {
    fn read_slice(&mut self, offset: u64, data: &mut [u8]) -> Result<(), ArmError> {
        // When responding, the probe will prefix the response with b"&K", and will
        // suffix the response with b"#\0". Each byte is encoded as a hex pair.
        // Ensure the buffer passed to us can accommodate these extra four bytes
        // as well as the double-width encoded bytes.
        if data.len() * 2 + 4 >= super::BLACK_MAGIC_REMOTE_SIZE_MAX {
            return Err(ArmError::OutOfBounds);
        }
        let command = match self.probe.probe.remote_protocol {
            ProtocolVersion::V0 => {
                return Err(ArmError::Probe(
                    DebugProbeError::CommandNotSupportedByProbe {
                        command_name: "adiv5 memory read",
                    },
                ));
            }
            ProtocolVersion::V0P => RemoteCommand::MemReadV0P {
                apsel: self.apsel,
                csw: self.csw,
                offset: offset
                    .try_into()
                    .map_err(|_| ArmError::AddressOutOf32BitAddressSpace)?,
                data,
            },
            ProtocolVersion::V1 | ProtocolVersion::V2 => RemoteCommand::MemReadV1 {
                index: self.index,
                apsel: self.apsel,
                csw: self.csw,
                offset: offset
                    .try_into()
                    .map_err(|_| ArmError::AddressOutOf32BitAddressSpace)?,
                data,
            },
            ProtocolVersion::V3 => RemoteCommand::MemReadV3 {
                index: self.index,
                apsel: self.apsel,
                csw: self.csw,
                offset: offset
                    .try_into()
                    .map_err(|_| ArmError::AddressOutOf32BitAddressSpace)?,
                data,
            },
            ProtocolVersion::V4 => RemoteCommand::MemReadV4 {
                index: self.index,
                apsel: self.apsel,
                csw: self.csw,
                offset,
                data,
            },
        };
        self.probe
            .probe
            .command(command)
            .map_err(|e| ArmError::Probe(e.into()))?;
        Ok(())
    }

    fn read(&mut self, offset: u64, data: &mut [u8]) -> Result<(), ArmError> {
        let chunk_size = super::BLACK_MAGIC_REMOTE_SIZE_MAX / 2 - 8;
        for (chunk_index, chunk) in data.chunks_mut(chunk_size).enumerate() {
            self.read_slice(chunk_index as u64 * chunk_size as u64 + offset, chunk)?;
        }
        Ok(())
    }

    fn write_slice(&mut self, align: Align, offset: u64, data: &[u8]) -> Result<(), ArmError> {
        // The Black Magic Probe as a 1024-byte buffer. The largest possible message is
        // b"!AM{:02x}{:02x}{:08x}{:02x}{:016x}{:08x}{DATA}#", which is 42 bytes long,
        // plus widening the actual data. Ensure the message getting sent to the device
        // doesn't exceed this.
        if data.len() * 2 + 42 >= super::BLACK_MAGIC_REMOTE_SIZE_MAX {
            return Err(ArmError::OutOfBounds);
        }
        let command = match self.probe.probe.remote_protocol {
            ProtocolVersion::V0 => {
                return Err(ArmError::Probe(
                    DebugProbeError::CommandNotSupportedByProbe {
                        command_name: "adiv5 memory write",
                    },
                ));
            }
            ProtocolVersion::V0P => RemoteCommand::MemWriteV0P {
                apsel: self.apsel,
                csw: self.csw,
                align,
                offset: offset
                    .try_into()
                    .map_err(|_| ArmError::AddressOutOf32BitAddressSpace)?,
                data,
            },
            ProtocolVersion::V1 | ProtocolVersion::V2 => RemoteCommand::MemWriteV1 {
                index: self.index,
                apsel: self.apsel,
                csw: self.csw,
                align,
                offset: offset
                    .try_into()
                    .map_err(|_| ArmError::AddressOutOf32BitAddressSpace)?,
                data,
            },
            ProtocolVersion::V3 => RemoteCommand::MemWriteV3 {
                index: self.index,
                apsel: self.apsel,
                csw: self.csw,
                align,
                offset: offset
                    .try_into()
                    .map_err(|_| ArmError::AddressOutOf32BitAddressSpace)?,
                data,
            },
            ProtocolVersion::V4 => RemoteCommand::MemWriteV4 {
                index: self.index,
                apsel: self.apsel,
                csw: self.csw,
                align,
                offset,
                data,
            },
        };
        let result = self
            .probe
            .probe
            .command(command)
            .map_err(|e| ArmError::Probe(e.into()))?
            .0;
        if result == 0 {
            Ok(())
        } else {
            Err(ArmError::Probe(DebugProbeError::Other(format!(
                "probe returned unexpected result: {}",
                result
            ))))
        }
    }

    fn write(&mut self, align: Align, offset: u64, data: &[u8]) -> Result<(), ArmError> {
        let word_size = 1 << (align as u8);
        let chunk_size = word_size * ((super::BLACK_MAGIC_REMOTE_SIZE_MAX / 2 - 42) / word_size);
        for (chunk_index, chunk) in data.chunks(chunk_size).enumerate() {
            self.write_slice(
                align,
                chunk_index as u64 * chunk_size as u64 + offset,
                chunk,
            )?;
        }
        Ok(())
    }
}

impl MemoryInterface<ArmError> for BlackMagicProbeMemoryInterface<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        self.probe.probe.remote_protocol == ProtocolVersion::V4
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), ArmError> {
        self.read(address, data.as_mut_bytes())
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
        self.read(address, data.as_mut_bytes())
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), ArmError> {
        self.read(address, data.as_mut_bytes())
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError> {
        self.read(address, data.as_mut_bytes())
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), ArmError> {
        self.write(Align::U64, address, data.as_bytes())
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError> {
        self.write(Align::U32, address, data.as_bytes())
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), ArmError> {
        self.write(Align::U16, address, data.as_bytes())
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError> {
        self.write(Align::U8, address, data.as_bytes())
    }

    fn supports_8bit_transfers(&self) -> Result<bool, ArmError> {
        Ok(true)
    }

    fn flush(&mut self) -> Result<(), ArmError> {
        Ok(())
    }
}
