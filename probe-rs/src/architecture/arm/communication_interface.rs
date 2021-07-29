use super::{
    ap::{
        valid_access_ports, AccessPort, ApAccess, ApClass, BaseaddrFormat, GenericAp, MemoryAp,
        BASE, BASE2, CSW, IDR,
    },
    dp::{Abort, Ctrl, DebugPortError, DebugPortId, DebugPortVersion, DpAccess, Select, DPIDR},
    memory::{adi_v5_memory_interface::ADIMemoryInterface, Component},
    ApAddress, DapAccess, DpAddress, PortType, RawDapAccess, SwoAccess, SwoConfig,
};
use crate::{
    architecture::arm::ap::DataSize, CommunicationInterface, DebugProbe, DebugProbeError,
    Error as ProbeRsError, Memory, Probe,
};
use anyhow::anyhow;
use jep106::JEP106Code;

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum DapError {
    #[error("An error occured in the SWD communication between probe and device.")]
    SwdProtocol,
    #[error("Target device did not respond to request.")]
    NoAcknowledge,
    #[error("Target device responded with FAULT response to request.")]
    FaultResponse,
    #[error("Target device responded with WAIT response to request.")]
    WaitResponse,
    #[error("Target power-up failed.")]
    TargetPowerUpFailed,
    #[error("Incorrect parity on READ request.")]
    IncorrectParity,
}

impl From<DapError> for DebugProbeError {
    fn from(error: DapError) -> Self {
        DebugProbeError::ArchitectureSpecific(Box::new(error))
    }
}

use std::{collections::HashMap, fmt::Debug, time::Duration};

pub trait Register: Clone + From<u32> + Into<u32> + Sized + Debug {
    const ADDRESS: u8;
    const NAME: &'static str;
}

pub trait ArmProbeInterface: DapAccess + SwoAccess + Debug + Send {
    fn memory_interface(&mut self, access_port: MemoryAp) -> Result<Memory<'_>, ProbeRsError>;

    fn ap_information(&mut self, access_port: GenericAp) -> Result<&ApInformation, ProbeRsError>;

    fn num_access_ports(&mut self, dp: DpAddress) -> Result<usize, ProbeRsError>;

    fn read_from_rom_table(&mut self, dp: DpAddress) -> Result<Option<ArmChipInfo>, ProbeRsError>;

    /// Deassert the target reset line
    ///
    /// When connecting under reset,
    /// initial configuration is done with the reset line
    /// asserted. After initial configuration is done, the
    /// reset line can be deasserted using this method.
    ///
    /// See also [`Probe::target_reset_deassert`].
    fn target_reset_deassert(&mut self) -> Result<(), ProbeRsError>;

    fn close(self: Box<Self>) -> Probe;
}

#[derive(Debug)]
pub(crate) struct ArmCommunicationInterfaceState {
    current_dp: Option<DpAddress>,
    dps: HashMap<DpAddress, DpState>,
}

impl ArmCommunicationInterfaceState {
    pub fn new() -> Self {
        Self {
            current_dp: None,
            dps: HashMap::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct DpState {
    pub debug_port_version: DebugPortVersion,

    pub current_dpbanksel: u8,

    pub current_apsel: u8,
    pub current_apbanksel: u8,

    /// Information about the APs of the target.
    /// APs are identified by a number, starting from zero.
    pub ap_information: Vec<ApInformation>,
}

impl DpState {
    pub fn new() -> Self {
        Self {
            debug_port_version: DebugPortVersion::Unsupported(0xFF),
            current_dpbanksel: 0,
            current_apsel: 0,
            current_apbanksel: 0,
            ap_information: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum ApInformation {
    /// Information about a Memory AP, which allows access to target memory. See Chapter C2 in the [ARM Debug Interface Architecture Specification].
    ///
    /// [ARM Debug Interface Architecture Specification]: https://developer.arm.com/documentation/ihi0031/d/
    MemoryAp(MemoryApInformation),
    /// Information about an AP with an unknown class.
    Other {
        /// Zero-based port number of the access port. This is used in the debug port to select an AP.
        address: ApAddress,
    },
}

impl ApInformation {
    /// Read information about an AP from its registers.
    ///
    /// This reads the IDR register of the AP, and parses
    /// further AP specific information based on its class.
    ///
    /// Currently, AP specific information is read for Memory APs.
    pub(crate) fn read_from_target<P>(
        probe: &mut P,
        access_port: GenericAp,
    ) -> Result<Self, DebugProbeError>
    where
        P: ApAccess,
    {
        let idr: IDR = probe.read_ap_register(access_port)?;

        if idr.CLASS == ApClass::MemAp {
            let access_port: MemoryAp = access_port.into();

            let base_register: BASE = probe.read_ap_register(access_port)?;

            let mut base_address = if BaseaddrFormat::ADIv5 == base_register.Format {
                let base2: BASE2 = probe.read_ap_register(access_port)?;

                u64::from(base2.BASEADDR) << 32
            } else {
                0
            };
            base_address |= u64::from(base_register.BASEADDR << 12);

            // Save old CSW value. STLink firmare caches it, which breaks things
            // if we change it behind its back.
            let old_csw: CSW = probe.read_ap_register(access_port)?;

            // Read information about HNONSEC support and supported access widths
            let csw = CSW::new(DataSize::U8);

            probe.write_ap_register(access_port, csw)?;
            let csw: CSW = probe.read_ap_register(access_port)?;

            probe.write_ap_register(access_port, old_csw)?;

            let only_32bit_data_size = csw.SIZE != DataSize::U8;

            let supports_hnonsec = csw.HNONSEC == 1;

            log::debug!("HNONSEC supported: {}", supports_hnonsec);

            Ok(ApInformation::MemoryAp(MemoryApInformation {
                address: access_port.ap_address(),
                only_32bit_data_size,
                debug_base_address: base_address,
                supports_hnonsec,
            }))
        } else {
            Ok(ApInformation::Other {
                address: access_port.ap_address(),
            })
        }
    }
}

#[derive(Debug, Clone)]
pub struct MemoryApInformation {
    /// Zero-based port number of the access port. This is used in the debug port to select an AP.
    pub address: ApAddress,
    /// Some Memory APs only support 32 bit wide access to data, while others
    /// also support other widths. Based on this, 8 bit data access can either
    /// be performed directly, or has to be done as a 32 bit access.
    pub only_32bit_data_size: bool,
    /// The Debug Base Address points to either the start of a set of debug register,
    /// or a ROM table which describes the connected debug components.
    ///
    /// See chapter C2.6, [ARM Debug Interface Architecture Specification].
    ///
    /// [ARM Debug Interface Architecture Specification]: https://developer.arm.com/documentation/ihi0031/d/
    pub debug_base_address: u64,

    /// Indicates if the HNONSEC bit in the CSW register is supported.
    /// See section E1.5.1, [ARM Debug Interface Architecture Specification].
    ///
    /// [ARM Debug Interface Architecture Specification]: https://developer.arm.com/documentation/ihi0031/d/
    pub supports_hnonsec: bool,
}

#[derive(Debug)]
pub struct ArmCommunicationInterface {
    probe: Box<dyn DapProbe>,
    state: ArmCommunicationInterfaceState,
    use_overrun_detect: bool,
}

/// Helper trait for probes which offer access to ARM DAP (Debug Access Port).
///
/// This is used to combine the traits, because it cannot be done in the ArmCommunicationInterface
/// struct itself.
pub trait DapProbe: RawDapAccess + DebugProbe {}

impl ArmProbeInterface for ArmCommunicationInterface {
    fn memory_interface(&mut self, access_port: MemoryAp) -> Result<Memory<'_>, ProbeRsError> {
        ArmCommunicationInterface::memory_interface(self, access_port)
    }

    fn ap_information(&mut self, access_port: GenericAp) -> Result<&ApInformation, ProbeRsError> {
        ArmCommunicationInterface::ap_information(self, access_port)
    }

    fn read_from_rom_table(&mut self, dp: DpAddress) -> Result<Option<ArmChipInfo>, ProbeRsError> {
        ArmCommunicationInterface::read_from_rom_table(self, dp)
    }

    fn num_access_ports(&mut self, dp: DpAddress) -> Result<usize, ProbeRsError> {
        ArmCommunicationInterface::num_access_ports(self, dp)
    }

    fn target_reset_deassert(&mut self) -> Result<(), ProbeRsError> {
        self.probe.target_reset_deassert()?;

        Ok(())
    }

    fn close(self: Box<Self>) -> Probe {
        Probe::from_attached_probe(self.probe.into_probe())
    }
}

impl<'interface> ArmCommunicationInterface {
    pub(crate) fn new(
        probe: Box<dyn DapProbe>,
        use_overrun_detect: bool,
    ) -> Result<Self, (Box<dyn DapProbe>, DebugProbeError)> {
        Ok(Self {
            probe,
            state: ArmCommunicationInterfaceState::new(),
            use_overrun_detect,
        })
    }

    pub fn memory_interface(
        &'interface mut self,
        access_port: MemoryAp,
    ) -> Result<Memory<'interface>, ProbeRsError> {
        let info = self.ap_information(access_port).map_err(|_| {
            anyhow!(
                "Failed to get information for AP {:x?}",
                access_port.ap_address()
            )
        })?;

        match info {
            ApInformation::MemoryAp(ap_information) => {
                let information = ap_information.clone();
                let adi_v5_memory_interface = ADIMemoryInterface::<
                    'interface,
                    ArmCommunicationInterface,
                >::new(self, &information)
                .map_err(ProbeRsError::architecture_specific)?;

                Ok(Memory::new(adi_v5_memory_interface, access_port))
            }
            ApInformation::Other { address } => Err(ProbeRsError::Other(anyhow!(format!(
                "AP {:x?} is not a memory AP",
                address
            )))),
        }
    }

    fn init_dp(&mut self, dp: DpAddress) -> Result<(), DebugProbeError> {
        // Assume that we have DebugPort v1 Interface!
        // Maybe change this in the future when other versions are released.

        log::debug!("Initializing DP {:x?}", dp);

        self.state.dps.insert(dp, DpState::new());

        // Read the DP ID.
        let dpidr: DPIDR = self.read_dp_register(dp)?;

        // Check the version of debug port used
        let debug_port_version = DebugPortVersion::from(dpidr.version());
        log::debug!("Debug Port version: {:?}", debug_port_version);
        // note(unwrap): we have inserted the state above, it must exist.
        let state = self.state.dps.get_mut(&dp).unwrap();
        state.debug_port_version = debug_port_version;

        let dp_id: DebugPortId = dpidr.into();
        log::debug!("DebugPort ID:  {:#x?}", dp_id);

        // Clear all existing sticky errors.
        let mut abort_reg = Abort(0);
        abort_reg.set_orunerrclr(true);
        abort_reg.set_wderrclr(true);
        abort_reg.set_stkerrclr(true);
        abort_reg.set_stkcmpclr(true);
        self.write_dp_register(dp, abort_reg)?;

        // Select the DPBANK[0].
        // This is most likely not required but still good practice.
        let mut select_reg = Select(0);
        select_reg.set_dp_bank_sel(0);
        self.write_dp_register(dp, select_reg)?; // select DBPANK 0

        // Power up the system, such that we can actually work with it!
        log::debug!("Requesting debug power");
        let mut ctrl_reg = Ctrl::default();
        ctrl_reg.set_csyspwrupreq(true);
        ctrl_reg.set_cdbgpwrupreq(true);
        ctrl_reg.set_orun_detect(self.use_overrun_detect);
        self.write_dp_register(dp, ctrl_reg)?;

        // Check the return value to see whether power up was ok.
        let ctrl_reg: Ctrl = self.read_dp_register(dp)?;
        if !(ctrl_reg.csyspwrupack() && ctrl_reg.cdbgpwrupack()) {
            log::error!("Debug power request failed");
            return Err(DapError::TargetPowerUpFailed.into());
        }

        /* determine the number and type of available APs */
        log::trace!("Searching valid APs");

        for ap in valid_access_ports(self, dp) {
            let ap_state = ApInformation::read_from_target(self, ap)?;
            log::debug!("AP {:x?}: {:?}", ap, ap_state);

            // note(unwrap): we have inserted the state above, it must exist.
            let state = self.state.dps.get_mut(&dp).unwrap();
            state.ap_information.push(ap_state);
        }

        Ok(())
    }

    fn select_dp(&mut self, dp: DpAddress) -> Result<(), DebugProbeError> {
        if self.state.current_dp == Some(dp) {
            return Ok(());
        }

        log::debug!("Selecting DP {:x?}", dp);

        if let Err(e) = self.probe.select_dp(dp) {
            self.state.current_dp = None;
            Err(e)?;
        }

        self.state.current_dp = Some(dp);

        if !self.state.dps.contains_key(&dp) {
            self.init_dp(dp)?;
        }

        Ok(())
    }

    fn select_dp_and_dp_bank(
        &mut self,
        dp: DpAddress,
        dp_register_address: u8,
    ) -> Result<(), DebugPortError> {
        self.select_dp(dp)?;

        // NOTE(unwrap): select_dp adds the dp to state if not present.
        let dp_state = self.state.dps.get_mut(&dp).unwrap();

        // DP register addresses are 4 bank bits, 4 address bits. Lowest 2 address bits are
        // always 0, so this leaves only 4 possible addresses: 0x0, 0x4, 0x8, 0xC.
        // Only address 0x4 is banked, the rest are don't care.

        let bank = dp_register_address >> 4;
        let addr = dp_register_address & 0xF;

        if addr != 4 {
            return Ok(());
        }

        if bank != dp_state.current_dpbanksel {
            dp_state.current_dpbanksel = bank;

            let mut select = Select(0);

            log::debug!("Changing DP_BANK_SEL to {}", dp_state.current_dpbanksel);

            select.set_ap_sel(dp_state.current_apsel);
            select.set_ap_bank_sel(dp_state.current_apbanksel);
            select.set_dp_bank_sel(dp_state.current_dpbanksel);

            self.write_dp_register(dp, select)?;
        }

        Ok(())
    }

    fn select_ap_and_ap_bank(
        &mut self,
        ap: ApAddress,
        ap_register_address: u8,
    ) -> Result<(), DebugProbeError> {
        self.select_dp(ap.dp)?;

        // NOTE(unwrap): select_dp adds the dp to state if not present.
        let dp_state = self.state.dps.get_mut(&ap.dp).unwrap();

        let port = ap.ap;
        let ap_bank = ap_register_address >> 4;

        let mut cache_changed = if dp_state.current_apsel != port {
            dp_state.current_apsel = port;
            true
        } else {
            false
        };

        if dp_state.current_apbanksel != ap_bank {
            dp_state.current_apbanksel = ap_bank;
            cache_changed = true;
        }

        if cache_changed {
            let mut select = Select(0);

            log::debug!(
                "Changing AP to {}, AP_BANK_SEL to {}",
                dp_state.current_apsel,
                dp_state.current_apbanksel
            );

            select.set_ap_sel(dp_state.current_apsel);
            select.set_ap_bank_sel(dp_state.current_apbanksel);
            select.set_dp_bank_sel(dp_state.current_dpbanksel);

            self.write_dp_register(ap.dp, select)?;
        }

        Ok(())
    }

    /// Determine the type and additional information about an AP.
    pub(crate) fn ap_information(
        &mut self,
        access_port: impl AccessPort,
    ) -> Result<&ApInformation, ProbeRsError> {
        let addr = access_port.ap_address();
        self.select_dp(addr.dp)?;

        let state = self.state.dps.get(&addr.dp).unwrap();

        match state.ap_information.get(addr.ap as usize) {
            Some(res) => Ok(res),
            None => Err(anyhow!("AP {:x?} does not exist", addr).into()),
        }
    }

    fn num_access_ports(&mut self, dp: DpAddress) -> Result<usize, ProbeRsError> {
        self.select_dp(dp)?;

        let state = self.state.dps.get(&dp).unwrap();
        Ok(state.ap_information.len())
    }
}

impl CommunicationInterface for ArmCommunicationInterface {
    fn flush(&mut self) -> Result<(), DebugProbeError> {
        self.probe.raw_flush()
    }
}

impl SwoAccess for ArmCommunicationInterface {
    fn enable_swo(&mut self, config: &SwoConfig) -> Result<(), ProbeRsError> {
        match self.probe.get_swo_interface_mut() {
            Some(interface) => interface.enable_swo(config),
            None => Err(ProbeRsError::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }

    fn disable_swo(&mut self) -> Result<(), ProbeRsError> {
        match self.probe.get_swo_interface_mut() {
            Some(interface) => interface.disable_swo(),
            None => Err(ProbeRsError::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }

    fn read_swo_timeout(&mut self, timeout: Duration) -> Result<Vec<u8>, ProbeRsError> {
        match self.probe.get_swo_interface_mut() {
            Some(interface) => interface.read_swo_timeout(timeout),
            None => Err(ProbeRsError::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }
}

impl DapAccess for ArmCommunicationInterface {
    fn read_raw_dp_register(&mut self, dp: DpAddress, address: u8) -> Result<u32, DebugProbeError> {
        self.select_dp_and_dp_bank(dp, address)?;
        let result = self.probe.raw_read_register(PortType::DebugPort, address)?;
        Ok(result)
    }

    fn write_raw_dp_register(
        &mut self,
        dp: DpAddress,
        address: u8,
        value: u32,
    ) -> Result<(), DebugProbeError> {
        self.select_dp_and_dp_bank(dp, address)?;
        self.probe
            .raw_write_register(PortType::DebugPort, address, value)?;
        Ok(())
    }

    fn read_raw_ap_register(&mut self, ap: ApAddress, address: u8) -> Result<u32, DebugProbeError> {
        self.select_ap_and_ap_bank(ap, address)?;

        let result = self
            .probe
            .raw_read_register(PortType::AccessPort, address)?;

        Ok(result)
    }

    fn read_raw_ap_register_repeated(
        &mut self,
        ap: ApAddress,
        address: u8,
        values: &mut [u32],
    ) -> Result<(), DebugProbeError> {
        self.select_ap_and_ap_bank(ap, address)?;

        self.probe
            .raw_read_block(PortType::AccessPort, address, values)?;
        Ok(())
    }

    fn write_raw_ap_register(
        &mut self,
        ap: ApAddress,
        address: u8,
        value: u32,
    ) -> Result<(), DebugProbeError> {
        self.select_ap_and_ap_bank(ap, address)?;

        self.probe
            .raw_write_register(PortType::AccessPort, address, value)
    }

    fn write_raw_ap_register_repeated(
        &mut self,
        ap: ApAddress,
        address: u8,
        values: &[u32],
    ) -> Result<(), DebugProbeError> {
        self.select_ap_and_ap_bank(ap, address)?;

        self.probe
            .raw_write_block(PortType::AccessPort, address, values)?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct ArmChipInfo {
    pub manufacturer: JEP106Code,
    pub part: u16,
}

impl ArmCommunicationInterface {
    pub fn read_from_rom_table(
        &mut self,
        dp: DpAddress,
    ) -> Result<Option<ArmChipInfo>, ProbeRsError> {
        // faults on some chips need to be cleaned up.
        let aps = valid_access_ports(self, dp);

        // Check sticky error and cleanup if necessary
        let ctrl_reg: crate::architecture::arm::dp::Ctrl = self
            .read_dp_register(dp)
            .map_err(ProbeRsError::architecture_specific)?;

        if ctrl_reg.sticky_err() {
            log::trace!("AP Search faulted. Cleaning up");
            let mut abort = Abort::default();
            abort.set_stkerrclr(true);
            self.write_dp_register(dp, abort)
                .map_err(ProbeRsError::architecture_specific)?;
        }
        for access_port in aps {
            let idr: IDR = self
                .read_ap_register(access_port)
                .map_err(ProbeRsError::Probe)?;
            log::debug!("{:#x?}", idr);

            if idr.CLASS == ApClass::MemAp {
                let access_port: MemoryAp = access_port.into();

                let baseaddr = access_port.base_address(self)?;

                let mut memory = self
                    .memory_interface(access_port)
                    .map_err(ProbeRsError::architecture_specific)?;

                let component = Component::try_parse(&mut memory, baseaddr)
                    .map_err(ProbeRsError::architecture_specific)?;

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
        // log::info!(
        //     "{}\n{}\n{}\n{}",
        //     "If you are using a Nordic chip, it might be locked to debug access".yellow(),
        //     "Run cargo flash with --nrf-recover to unlock".yellow(),
        //     "WARNING: --nrf-recover will erase the entire code".yellow(),
        //     "flash and UICR area of the device, in addition to the entire RAM".yellow()
        // );

        Ok(None)
    }
}

impl std::fmt::Display for ArmChipInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let manu = match self.manufacturer.get() {
            Some(name) => name.to_string(),
            None => format!(
                "<unknown manufacturer (cc={:2x}, id={:2x})>",
                self.manufacturer.cc, self.manufacturer.id
            ),
        };
        write!(f, "{} 0x{:04x}", manu, self.part)
    }
}
