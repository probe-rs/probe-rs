use super::{
    ap::{
        valid_access_ports, AccessPort, ApAccess, ApClass, BaseaddrFormat, GenericAp, MemoryAp,
        BASE, BASE2, CFG, CSW, IDR,
    },
    dp::{Abort, Ctrl, DebugPortVersion, DpAccess, Select, DPIDR},
    memory::{
        adi_v5_memory_interface::{ADIMemoryInterface, ArmProbe},
        Component,
    },
    sequences::{ArmDebugSequence, DefaultArmSequence},
    ApAddress, ArmError, DapAccess, DpAddress, PortType, RawDapAccess, SwoAccess, SwoConfig,
};
use crate::{
    architecture::arm::ap::DataSize, DebugProbe, DebugProbeError, Error as ProbeRsError, Probe,
};
use jep106::JEP106Code;

use std::{
    collections::{hash_map, HashMap},
    fmt::Debug,
    sync::Arc,
    time::Duration,
};

/// An error in the communication with an access port or
/// debug port.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum DapError {
    /// An error occurred during SWD communication.
    #[error("An error occurred in the SWD communication between probe and device.")]
    SwdProtocol,
    /// The target device did not respond to the request.
    #[error("Target device did not respond to request.")]
    NoAcknowledge,
    /// The target device responded with a FAULT response to the request.
    #[error("Target device responded with a FAULT response to the request.")]
    FaultResponse,
    /// Target device responded with a WAIT response to the request.
    #[error("Target device responded with a WAIT response to the request.")]
    WaitResponse,
    /// The parity bit on the read request was incorrect.
    #[error("Incorrect parity on READ request.")]
    IncorrectParity,
}

/// A trait to be implemented on register types for typed device access.
pub trait Register:
    Clone + TryFrom<u32, Error = RegisterParseError> + Into<u32> + Sized + Debug
{
    /// The address of the register (in bytes).
    const ADDRESS: u8;
    /// The name of the register as string.
    const NAME: &'static str;
}

#[derive(Debug, thiserror::Error)]
#[error("Failed to parse register {name} from {value:#010x}")]
pub struct RegisterParseError {
    name: &'static str,
    value: u32,
}

impl RegisterParseError {
    pub fn new(name: &'static str, value: u32) -> Self {
        RegisterParseError { name, value }
    }
}

/// To be implemented by debug probe drivers that support debugging ARM cores.
pub trait ArmProbeInterface: DapAccess + SwdSequence + SwoAccess + Send {
    /// Returns a memory interface to access the target's memory.
    fn memory_interface(
        &mut self,
        access_port: MemoryAp,
    ) -> Result<Box<dyn ArmProbe + '_>, ArmError>;

    /// Returns information about a specific access port.
    fn ap_information(&mut self, access_port: GenericAp) -> Result<&ApInformation, ArmError>;

    /// Returns the number of access ports the debug port has.
    ///
    /// If the target device has multiple debug ports, this will switch the active debug port
    /// if necessary. This will also  
    fn num_access_ports(&mut self, dp: DpAddress) -> Result<usize, ArmError>;

    /// Reads the chip info from the romtable of given debug port.
    fn read_chip_info_from_rom_table(
        &mut self,
        dp: DpAddress,
    ) -> Result<Option<ArmChipInfo>, ArmError>;

    /// Closes the interface and returns back the generic probe it consumed.
    fn close(self: Box<Self>) -> Probe;
}

// TODO: Rename trait!
pub trait SwdSequence {
    /// Corresponds to the DAP_SWJ_Sequence function from the ARM Debug sequences
    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError>;

    /// Corresponds to the DAP_SWJ_Pins function from the ARM Debug sequences
    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError>;
}

pub trait UninitializedArmProbe: SwdSequence + Debug {
    fn initialize(
        self: Box<Self>,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Box<dyn ArmProbeInterface>, (Box<dyn UninitializedArmProbe>, ProbeRsError)>;

    fn initialize_unspecified(
        self: Box<Self>,
    ) -> Result<Box<dyn ArmProbeInterface>, (Box<dyn UninitializedArmProbe>, ProbeRsError)> {
        self.initialize(DefaultArmSequence::create())
    }

    /// Closes the interface and returns back the generic probe it consumed.
    fn close(self: Box<Self>) -> Probe;
}

pub trait ArmDebugState {}

#[derive(Debug)]
pub struct Uninitialized {
    /// Specify if overrun detect should be enabled when the probe is initialized.
    pub(crate) use_overrun_detect: bool,
}

pub struct Initialized {
    /// Currently selected debug port. For targets without multidrop,
    /// this will always be the single, default debug port in the system.
    current_dp: Option<DpAddress>,
    dps: HashMap<DpAddress, DpState>,
    use_overrun_detect: bool,
    sequence: Arc<dyn ArmDebugSequence>,
}

impl Initialized {
    pub fn new(sequence: Arc<dyn ArmDebugSequence>, use_overrun_detect: bool) -> Self {
        Self {
            current_dp: None,
            dps: HashMap::new(),
            use_overrun_detect,
            sequence,
        }
    }
}

impl ArmDebugState for Uninitialized {}

impl ArmDebugState for Initialized {}

#[derive(Debug)]
pub(crate) struct DpState {
    pub _debug_port_version: DebugPortVersion,

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
            _debug_port_version: DebugPortVersion::Unsupported(0xFF),
            current_dpbanksel: 0,
            current_apsel: 0,
            current_apbanksel: 0,
            ap_information: Vec::new(),
        }
    }
}

/// Information about an access port. Can be used for target discovery.
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
        /// Content of the [`IDR`] register describing this AP.
        idr: IDR,
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
    ) -> Result<Self, ArmError>
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

            tracing::debug!("HNONSEC supported: {}", supports_hnonsec);

            let device_enabled = csw.DeviceEn == 1;

            tracing::debug!("Device enabled: {}", device_enabled);

            let cfg: CFG = probe.read_ap_register(access_port)?;

            let has_large_address_extension = cfg.LA == 1;
            let has_large_data_extension = cfg.LD == 1;

            Ok(ApInformation::MemoryAp(MemoryApInformation {
                address: access_port.ap_address(),
                supports_only_32bit_data_size: only_32bit_data_size,
                debug_base_address: base_address,
                supports_hnonsec,
                has_large_address_extension,
                has_large_data_extension,
                device_enabled,
            }))
        } else {
            Ok(ApInformation::Other {
                address: access_port.ap_address(),
                idr,
            })
        }
    }
}

/// Information about a memory access port. Can be used for target discovery.
/// Useful for detecting supported memory access of a target.
#[derive(Debug, Clone)]
pub struct MemoryApInformation {
    /// Zero-based port number of the access port. This is used in the debug port to select an AP.
    pub address: ApAddress,

    /// Some Memory APs only support 32 bit wide access to data, while others
    /// also support other widths. Based on this, 8 bit data access can either
    /// be performed directly, or has to be done as a 32 bit access.
    pub supports_only_32bit_data_size: bool,

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
    ///
    /// If HNONSEC is not supported, bit 30 in the CSW register has
    /// to be set to 1 at all times.
    pub supports_hnonsec: bool,

    /// This AP has the large address extension present, supporting 64-bit addresses
    pub has_large_address_extension: bool,

    /// This AP has the large data extension present, supporting 64-bit data access
    pub has_large_data_extension: bool,

    /// Memory transactions can be issued through this AP. If this bit is not set,
    /// no transactions can be issued.
    pub device_enabled: bool,
}

/// An implementation of the communication protocol between probe and target.
/// Can be used to perform all sorts of generic debug access on ARM targets with probes that support low level access.
/// (E.g. CMSIS-DAP and J-Link support this, ST-Link does not)
#[derive(Debug)]
pub struct ArmCommunicationInterface<S: ArmDebugState> {
    probe: Box<dyn DapProbe>,
    state: S,
}

/// Helper trait for probes which offer access to ARM DAP (Debug Access Port).
///
/// This is used to combine the traits, because it cannot be done in the ArmCommunicationInterface
/// struct itself.
pub trait DapProbe: RawDapAccess + DebugProbe {}

impl ArmProbeInterface for ArmCommunicationInterface<Initialized> {
    fn memory_interface(
        &mut self,
        access_port: MemoryAp,
    ) -> Result<Box<dyn ArmProbe + '_>, ArmError> {
        ArmCommunicationInterface::memory_interface(self, access_port)
    }

    fn ap_information(&mut self, access_port: GenericAp) -> Result<&ApInformation, ArmError> {
        let info = ArmCommunicationInterface::ap_information(self, access_port)?;

        info.ok_or_else(|| ArmError::ApDoesNotExist(access_port.ap_address()))
    }

    fn read_chip_info_from_rom_table(
        &mut self,
        dp: DpAddress,
    ) -> Result<Option<ArmChipInfo>, ArmError> {
        ArmCommunicationInterface::read_chip_info_from_rom_table(self, dp)
    }

    fn num_access_ports(&mut self, dp: DpAddress) -> Result<usize, ArmError> {
        ArmCommunicationInterface::num_access_ports(self, dp)
    }

    fn close(self: Box<Self>) -> Probe {
        Probe::from_attached_probe(RawDapAccess::into_probe(self.probe))
    }
}

impl<S: ArmDebugState> SwdSequence for ArmCommunicationInterface<S> {
    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        self.probe.swj_sequence(bit_len, bits)?;

        Ok(())
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

impl ArmCommunicationInterface<Uninitialized> {
    pub(crate) fn new(probe: Box<dyn DapProbe>, use_overrun_detect: bool) -> Self {
        let state = Uninitialized { use_overrun_detect };

        Self { probe, state }
    }

    fn into_initialized(
        self,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<ArmCommunicationInterface<Initialized>, (Box<Self>, DebugProbeError)> {
        let use_overrun_detect = self.state.use_overrun_detect;

        ArmCommunicationInterface::<Initialized>::from_uninitialized(
            self,
            sequence,
            use_overrun_detect,
        )
    }
}

impl UninitializedArmProbe for ArmCommunicationInterface<Uninitialized> {
    fn initialize(
        mut self: Box<Self>,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Box<dyn ArmProbeInterface>, (Box<dyn UninitializedArmProbe>, ProbeRsError)> {
        let setup_span = tracing::debug_span!("debug_port_setup").entered();
        if let Err(e) = sequence.debug_port_setup(&mut *self.probe) {
            return Err((self as Box<_>, e.into()));
        }

        drop(setup_span);

        let interface = self
            .into_initialized(sequence)
            .map_err(|(s, err)| (s as Box<_>, ProbeRsError::Probe(err)))?;

        Ok(Box::new(interface))
    }

    fn close(self: Box<Self>) -> Probe {
        Probe::from_attached_probe(RawDapAccess::into_probe(self.probe))
    }
}

impl<S: ArmDebugState> ArmCommunicationInterface<S> {
    fn _get_debug_port_version(&mut self) -> Result<DebugPortVersion, ArmError> {
        let dpidr = DPIDR(self.probe.raw_read_register(PortType::DebugPort, 0)?);

        Ok(DebugPortVersion::from(dpidr.version()))
    }
}

impl<'interface> ArmCommunicationInterface<Initialized> {
    fn from_uninitialized(
        interface: ArmCommunicationInterface<Uninitialized>,
        sequence: Arc<dyn ArmDebugSequence>,
        use_overrun_detect: bool,
    ) -> Result<
        Self,
        (
            Box<ArmCommunicationInterface<Uninitialized>>,
            DebugProbeError,
        ),
    > {
        let initialized_interface = ArmCommunicationInterface {
            probe: interface.probe,
            state: Initialized::new(sequence, use_overrun_detect),
        };

        Ok(initialized_interface)
    }

    /// Tries to obtain a memory interface which can be used to read memory from ARM targets.
    pub fn memory_interface(
        &'interface mut self,
        access_port: MemoryAp,
    ) -> Result<Box<dyn ArmProbe + 'interface>, ArmError> {
        let info = self
            .ap_information(access_port)?
            .ok_or_else(|| ArmError::ApDoesNotExist(access_port.ap_address()))?;

        match info {
            ApInformation::MemoryAp(ap_information) => {
                let information = ap_information.clone();
                let adi_v5_memory_interface = ADIMemoryInterface::<
                    'interface,
                    ArmCommunicationInterface<Initialized>,
                >::new(self, information)
                .map_err(|e| ArmError::from_access_port(e, access_port))?;

                Ok(Box::new(adi_v5_memory_interface))
            }
            ApInformation::Other { .. } => Err(ArmError::WrongApType),
        }
    }

    fn select_dp(&mut self, dp: DpAddress) -> Result<&mut DpState, ArmError> {
        if self.state.current_dp == Some(dp) {
            return Ok(self.state.dps.get_mut(&dp).unwrap());
        }

        tracing::debug!("Selecting DP {:x?}", dp);

        if let Err(e) = self.probe.select_dp(dp) {
            self.state.current_dp = None;
            return Err(e);
        }

        self.state.current_dp = Some(dp);

        if let hash_map::Entry::Vacant(entry) = self.state.dps.entry(dp) {
            let sequence = self.state.sequence.clone();

            entry.insert(DpState::new());

            let start_span = tracing::debug_span!("debug_port_start").entered();
            sequence.debug_port_start(self, dp)?;
            drop(start_span);

            // Make sure we enable the overrun detect mode when requested.
            // For "bit-banging" probes, such as JLink or FTDI, we rely on it for good, stable communication.
            // This is required as the default sequence (and most special implementations) does not do this.
            tracing::debug!("Setting orun_detect: {}", self.state.use_overrun_detect);
            let mut ctrl_reg: Ctrl = self.read_dp_register(dp)?;
            ctrl_reg.set_orun_detect(self.state.use_overrun_detect);
            self.write_dp_register(dp, ctrl_reg)?;

            /* determine the number and type of available APs */
            tracing::trace!("Searching valid APs");

            let ap_span = tracing::debug_span!("AP discovery").entered();
            for ap in valid_access_ports(self, dp) {
                let ap_state = ApInformation::read_from_target(self, ap)?;
                tracing::debug!("AP {:x?}: {:?}", ap, ap_state);

                // note(unwrap): we have inserted the state above, it must exist.
                let state = self.state.dps.get_mut(&dp).unwrap();
                state.ap_information.push(ap_state);
            }
            drop(ap_span);
        }

        // note(unwrap): Entry gets inserted above
        Ok(self.state.dps.get_mut(&dp).unwrap())
    }

    fn select_dp_and_dp_bank(
        &mut self,
        dp: DpAddress,
        dp_register_address: u8,
    ) -> Result<(), ArmError> {
        let dp_state = self.select_dp(dp)?;

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

            tracing::debug!("Changing DP_BANK_SEL to {}", dp_state.current_dpbanksel);

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
    ) -> Result<(), ArmError> {
        let dp_state = self.select_dp(ap.dp)?;

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

            tracing::debug!(
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
    ///
    /// If the AP doesn't exist, None is returned.
    pub(crate) fn ap_information(
        &mut self,
        access_port: impl AccessPort,
    ) -> Result<Option<&ApInformation>, ArmError> {
        let addr = access_port.ap_address();

        let state = self.select_dp(addr.dp)?;

        Ok(state.ap_information.get(addr.ap as usize))
    }

    fn num_access_ports(&mut self, dp: DpAddress) -> Result<usize, ArmError> {
        let state = self.select_dp(dp)?;

        Ok(state.ap_information.len())
    }
}

impl FlushableArmAccess for ArmCommunicationInterface<Initialized> {
    fn flush(&mut self) -> Result<(), ArmError> {
        self.probe.raw_flush()
    }

    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, DebugProbeError> {
        Ok(self)
    }
}

impl SwoAccess for ArmCommunicationInterface<Initialized> {
    fn enable_swo(&mut self, config: &SwoConfig) -> Result<(), ArmError> {
        match self.probe.get_swo_interface_mut() {
            Some(interface) => interface.enable_swo(config),
            None => Err(ArmError::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }

    fn disable_swo(&mut self) -> Result<(), ArmError> {
        match self.probe.get_swo_interface_mut() {
            Some(interface) => interface.disable_swo(),
            None => Err(ArmError::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }

    fn read_swo_timeout(&mut self, timeout: Duration) -> Result<Vec<u8>, ArmError> {
        match self.probe.get_swo_interface_mut() {
            Some(interface) => interface.read_swo_timeout(timeout),
            None => Err(ArmError::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }
}

impl DapAccess for ArmCommunicationInterface<Initialized> {
    fn read_raw_dp_register(&mut self, dp: DpAddress, address: u8) -> Result<u32, ArmError> {
        self.select_dp_and_dp_bank(dp, address)?;
        let result = self.probe.raw_read_register(PortType::DebugPort, address)?;
        Ok(result)
    }

    fn write_raw_dp_register(
        &mut self,
        dp: DpAddress,
        address: u8,
        value: u32,
    ) -> Result<(), ArmError> {
        self.select_dp_and_dp_bank(dp, address)?;
        self.probe
            .raw_write_register(PortType::DebugPort, address, value)?;
        Ok(())
    }

    fn read_raw_ap_register(
        &mut self,
        ap: ApAddress,
        address: u8,
    ) -> std::result::Result<u32, ArmError> {
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
    ) -> Result<(), ArmError> {
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
    ) -> Result<(), ArmError> {
        self.select_ap_and_ap_bank(ap, address)?;

        self.probe
            .raw_write_register(PortType::AccessPort, address, value)?;

        Ok(())
    }

    fn write_raw_ap_register_repeated(
        &mut self,
        ap: ApAddress,
        address: u8,
        values: &[u32],
    ) -> Result<(), ArmError> {
        self.select_ap_and_ap_bank(ap, address)?;

        self.probe
            .raw_write_block(PortType::AccessPort, address, values)?;
        Ok(())
    }
}

/// Information about the chip target we are currently attached to.
/// This can be used for discovery, tho, for now it does not work optimally,
/// as some manufacturers (e.g. ST Microelectronics) violate the spec and thus need special discovery procedures.
#[derive(Debug)]
pub struct ArmChipInfo {
    /// The JEP106 code of the manufacturer of this chip target.
    pub manufacturer: JEP106Code,
    /// The unique part numer of the chip target. Unfortunately this only unique in the spec.
    /// In practice some manufacturers violate the spec and assign a part number to an entire family.
    ///
    /// Consider this not unique when working with targets!
    pub part: u16,
}

impl ArmCommunicationInterface<Initialized> {
    /// Reads the chip info from the romtable of given debug port.
    pub fn read_chip_info_from_rom_table(
        &mut self,
        dp: DpAddress,
    ) -> Result<Option<ArmChipInfo>, ArmError> {
        // faults on some chips need to be cleaned up.
        let aps = valid_access_ports(self, dp);

        // Check sticky error and cleanup if necessary
        let ctrl_reg: crate::architecture::arm::dp::Ctrl = self.read_dp_register(dp)?;

        if ctrl_reg.sticky_err() {
            tracing::trace!("AP Search faulted. Cleaning up");
            let mut abort = Abort::default();
            abort.set_stkerrclr(true);
            self.write_dp_register(dp, abort)?;
        }
        for access_port in aps {
            let idr: IDR = self.read_ap_register(access_port)?;
            tracing::debug!("{:#x?}", idr);

            if idr.CLASS == ApClass::MemAp {
                let access_port: MemoryAp = access_port.into();

                let baseaddr = access_port.base_address(self)?;

                let mut memory = self.memory_interface(access_port)?;

                let component = Component::try_parse(&mut *memory, baseaddr)?;

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
        // tracing::info!(
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

/// A helper trait to get more specific interfaces.
pub trait FlushableArmAccess {
    /// Flush all remaining commands if the target driver implements batching.
    fn flush(&mut self) -> Result<(), ArmError>;

    /// Tries to get the underlying [`ArmCommunicationInterface`].
    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, DebugProbeError>;
}
