use crate::architecture::arm::ap::memory_ap::registers::{AddressIncrement, CSW};
use crate::architecture::arm::ap::memory_ap::{DataSize, MemoryAp, MemoryApType};
use crate::architecture::arm::ap::valid_access_ports;
use crate::architecture::arm::communication_interface::{Initialized, SwdSequence};
use crate::architecture::arm::dp::{Abort, Ctrl, DebugPortError, DpAccess, Select};
use crate::architecture::arm::memory::{ArmMemoryInterface, Component};
use crate::architecture::arm::{
    communication_interface::UninitializedArmProbe, sequences::ArmDebugSequence, ArmProbeInterface,
};
use crate::architecture::arm::{
    ArmChipInfo, ArmCommunicationInterface, ArmError, DapAccess, DpAddress,
    FullyQualifiedApAddress, RawDapAccess, SwoAccess,
};
use crate::probe::blackmagic::{Align, BlackMagicProbe, ProtocolVersion, RemoteCommand};
use crate::probe::{DebugProbeError, Probe};
use crate::{Error as ProbeRsError, MemoryInterface};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
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
    #[tracing::instrument(level = "trace", skip(self, sequence))]
    fn initialize(
        mut self: Box<Self>,
        sequence: Arc<dyn ArmDebugSequence>,
        dp: DpAddress,
    ) -> Result<Box<dyn ArmProbeInterface>, (Box<dyn UninitializedArmProbe>, ProbeRsError)> {
        // Switch to the correct mode
        if let Err(e) = sequence.debug_port_setup(&mut *self.probe, dp) {
            return Err((self, e.into()));
        }

        if let Err(e) = sequence.debug_port_connect(&mut *self.probe, dp) {
            tracing::warn!("failed to switch to DP {:x?}: {}", dp, e);

            // Try the more involved debug_port_setup sequence, which also handles dormant mode.
            if let Err(e) = sequence.debug_port_setup(&mut *self.probe, dp) {
                return Err((self, ProbeRsError::Arm(e)));
            }
        }

        let interface = BlackMagicProbeArmDebug::new(self.probe, dp)
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
    ) -> Result<Self, (Box<UninitializedBlackMagicArmProbe>, ArmError)> {
        let mut interface = Self {
            probe,
            access_ports: BTreeSet::new(),
        };

        interface.debug_port_start(dp).unwrap();

        interface.access_ports = valid_access_ports(&mut interface, DpAddress::Default)
            .into_iter()
            .collect();
        interface.access_ports.iter().for_each(|addr| {
            tracing::debug!("AP {:#x?}", addr);
        });
        Ok(interface)
    }

    /// Connect to the target debug port and power it up. This is based on the
    /// `DebugPortStart` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.html#debugPortStart
    fn debug_port_start(&mut self, dp: DpAddress) -> Result<(), ArmError> {
        // Clear all errors.
        // CMSIS says this is only necessary to do inside the `if powered_down`, but
        // without it here, nRF52840 faults in the next access.
        let mut abort = Abort(0);
        abort.set_dapabort(true);
        abort.set_orunerrclr(true);
        abort.set_wderrclr(true);
        abort.set_stkerrclr(true);
        abort.set_stkcmpclr(true);
        self.write_dp_register(dp, abort)?;

        self.write_dp_register(dp, Select(0))?;

        let ctrl = self.read_dp_register::<Ctrl>(dp)?;

        let powered_down = !(ctrl.csyspwrupack() && ctrl.cdbgpwrupack());

        if powered_down {
            let mut ctrl = Ctrl(0);
            ctrl.set_cdbgpwrupreq(true);
            ctrl.set_csyspwrupreq(true);
            self.write_dp_register(dp, ctrl.clone())?;

            let start = Instant::now();
            loop {
                let ctrl = self.read_dp_register::<Ctrl>(dp)?;
                if ctrl.csyspwrupack() && ctrl.cdbgpwrupack() {
                    break;
                }
                if start.elapsed() >= Duration::from_secs(1) {
                    return Err(ArmError::Timeout);
                }
                std::thread::sleep(Duration::from_millis(10));
            }

            // TODO: Handle JTAG Specific part

            // TODO: Only run the following code when the SWD protocol is used

            // Init AP Transfer Mode, Transaction Counter, and Lane Mask (Normal Transfer Mode, Include all Byte Lanes)
            let mut ctrl = Ctrl(0);
            ctrl.set_cdbgpwrupreq(true);
            ctrl.set_csyspwrupreq(true);
            ctrl.set_mask_lane(0b1111);
            self.write_dp_register(dp, ctrl)?;

            let ctrl_reg: Ctrl = self.read_dp_register(dp)?;
            if !(ctrl_reg.csyspwrupack() && ctrl_reg.cdbgpwrupack()) {
                tracing::error!("debug power-up request failed");
                return Err(DebugPortError::TargetPowerUpFailed.into());
            }

            // According to CMSIS docs, here's where we would clear errors
            // in ABORT, but we do that above instead.
        }
        Ok(())
    }
}

impl ArmProbeInterface for BlackMagicProbeArmDebug {
    fn access_ports(
        &mut self,
        dp: DpAddress,
    ) -> Result<BTreeSet<FullyQualifiedApAddress>, ArmError> {
        if dp != DpAddress::Default {
            return Err(ArmError::NotImplemented("multidrop not yet implemented"));
        }

        Ok(self.access_ports.clone())
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

    fn read_chip_info_from_rom_table(
        &mut self,
        dp: DpAddress,
    ) -> Result<Option<crate::architecture::arm::ArmChipInfo>, ArmError> {
        if dp != DpAddress::Default {
            return Err(ArmError::NotImplemented("multidrop not yet implemented"));
        }

        for ap in self.access_ports.clone() {
            if let Ok(mut memory) = self.memory_interface(&ap) {
                let base_address = memory.base_address()?;
                let component = Component::try_parse(&mut *memory, base_address)?;

                if let Component::Class1RomTable(component_id, _) = component {
                    if let Some(jep106) = component_id.peripheral_id().jep106() {
                        return Ok(Some(ArmChipInfo {
                            manufacturer: jep106,
                            part: component_id.peripheral_id().part(),
                        }));
                    }
                }
            }
        }

        Ok(None)
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

fn dp_to_bmp(dp: DpAddress) -> Result<u8, ArmError> {
    match dp {
        DpAddress::Default => Ok(0),
        DpAddress::Multidrop(val) => val.try_into().map_err(|_| ArmError::OutOfBounds),
    }
}

fn ap_to_bmp(ap: &FullyQualifiedApAddress) -> Result<(u8, u8), ArmError> {
    let apsel = match ap.ap() {
        crate::architecture::arm::ApAddress::V1(val) => *val,
        crate::architecture::arm::ApAddress::V2(_) => {
            return Err(ArmError::NotImplemented(
                "AP address v2 currently unsupported",
            ))
        }
    };
    Ok((dp_to_bmp(ap.dp())?, apsel))
}

impl DapAccess for BlackMagicProbeArmDebug {
    fn read_raw_dp_register(&mut self, dp: DpAddress, addr: u8) -> Result<u32, ArmError> {
        let index = dp_to_bmp(dp)?;
        let command = match self.probe.remote_protocol {
            ProtocolVersion::V0 => {
                return Err(ArmError::Probe(
                    DebugProbeError::CommandNotSupportedByProbe {
                        command_name: "adiv5 raw dp read",
                    },
                ));
            }
            ProtocolVersion::V0P => RemoteCommand::ReadDpV0P { addr },
            ProtocolVersion::V1 | ProtocolVersion::V2 => RemoteCommand::ReadDpV1 { index, addr },
            ProtocolVersion::V3 | ProtocolVersion::V4 => RemoteCommand::ReadDpV3 { index, addr },
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
        addr: u8,
        value: u32,
    ) -> Result<(), ArmError> {
        let index = dp_to_bmp(dp)?;
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
                addr,
                value,
            },
            ProtocolVersion::V1 | ProtocolVersion::V2 => RemoteCommand::RawAccessV1 {
                index,
                rnw: 0,
                addr,
                value,
            },
            ProtocolVersion::V3 | ProtocolVersion::V4 => RemoteCommand::RawAccessV3 {
                index,
                rnw: 0,
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

    fn read_raw_ap_register(
        &mut self,
        ap: &FullyQualifiedApAddress,
        addr: u8,
    ) -> Result<u32, ArmError> {
        let (index, apsel) = ap_to_bmp(ap)?;

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
        addr: u8,
        value: u32,
    ) -> Result<(), ArmError> {
        let (index, apsel) = ap_to_bmp(ap)?;
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
}

impl ArmMemoryInterface for BlackMagicProbeMemoryInterface<'_> {
    fn ap(&mut self) -> &mut MemoryAp {
        &mut self.current_ap
    }

    fn base_address(&mut self) -> Result<u64, ArmError> {
        self.current_ap.base_address(self.probe)
    }

    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, DebugProbeError> {
        Err(DebugProbeError::InterfaceNotAvailable {
            interface_name: "ARM",
        })
    }

    fn try_as_parts(
        &mut self,
    ) -> Result<(&mut ArmCommunicationInterface<Initialized>, &mut MemoryAp), DebugProbeError> {
        Err(DebugProbeError::InterfaceNotAvailable {
            interface_name: "ARM",
        })
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
        let chunk_size = super::BLACK_MAGIC_REMOTE_SIZE_MAX / 2 - 42;
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
