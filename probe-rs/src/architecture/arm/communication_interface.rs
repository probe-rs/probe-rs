use crate::{
    CoreStatus, Error,
    architecture::arm::{
        ApAddress, ArmError, DapAccess, FullyQualifiedApAddress, RawDapAccess, RegisterAddress,
        SwoAccess, SwoConfig, ap,
        dp::{Ctrl, DPIDR, DebugPortId, DebugPortVersion, DpAccess},
        dp::{DpAddress, DpRegisterAddress, Select1, SelectV1, SelectV3},
        memory::{ADIMemoryInterface, ArmMemoryInterface, Component},
        sequences::{ArmDebugSequence, DefaultArmSequence},
    },
    probe::{DebugProbe, DebugProbeError, Probe},
};
use jep106::JEP106Code;

use std::{
    collections::{BTreeSet, HashMap, hash_map},
    fmt::Debug,
    sync::Arc,
    time::Duration,
};

/// An error in the communication with an access port or
/// debug port.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq, Copy)]
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

/// To be implemented by debug probe drivers that support debugging ARM cores.
pub trait ArmProbeInterface: DapAccess + SwdSequence + SwoAccess + Send {
    /// Reinitialize the communication interface (in place).
    ///
    /// Some chip-specific reset sequences may disable the debug port. `reinitialize` allows
    /// a debug sequence to re-initialize the debug port, staying true to the `Initialized`
    /// type state.
    ///
    /// If you're invoking this from a debug sequence, know that `reinitialize` will likely
    /// call back onto you! Specifically, it will invoke some sequence of `debug_port_*`
    /// sequences with varying internal state. If you're not prepared for this, you might recurse.
    ///
    /// `reinitialize` does handle `debug_core_start` to re-initialize any core's debugging.
    /// If you're a chip-specific debug sequence, you're expected to handle this yourself.
    fn reinitialize(&mut self) -> Result<(), ArmError>;

    /// Returns a vector of all the access ports the current debug port has.
    ///
    /// If the target device has multiple debug ports, this will switch the active debug port
    /// if necessary.
    fn access_ports(
        &mut self,
        dp: DpAddress,
    ) -> Result<BTreeSet<FullyQualifiedApAddress>, ArmError>;

    /// Closes the interface and returns back the generic probe it consumed.
    fn close(self: Box<Self>) -> Probe;

    /// Return the currently connected debug port.
    fn current_debug_port(&self) -> DpAddress;

    /// Returns a memory interface to access the target's memory.
    fn memory_interface(
        &mut self,
        access_port: &FullyQualifiedApAddress,
    ) -> Result<Box<dyn ArmMemoryInterface + '_>, ArmError>;
}

/// Read chip information from the ROM tables
pub fn read_chip_info_from_rom_table(
    probe: &mut dyn ArmProbeInterface,
    dp: DpAddress,
) -> Result<Option<ArmChipInfo>, ArmError> {
    for ap in probe.access_ports(dp)? {
        if let Ok(mut memory) = probe.memory_interface(&ap) {
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
        dp: DpAddress,
    ) -> Result<Box<dyn ArmProbeInterface>, (Box<dyn UninitializedArmProbe>, Error)>;

    fn initialize_unspecified(
        self: Box<Self>,
        dp: DpAddress,
    ) -> Result<Box<dyn ArmProbeInterface>, (Box<dyn UninitializedArmProbe>, Error)> {
        self.initialize(DefaultArmSequence::create(), dp)
    }

    /// Closes the interface and returns back the generic probe it consumed.
    fn close(self: Box<Self>) -> Probe;
}

pub trait ArmDebugState {
    fn disconnect(&mut self, _probe: &mut dyn DapProbe) {}
}

#[derive(Debug)]
pub struct Uninitialized {
    /// Specify if overrun detect should be enabled when the probe is initialized.
    pub(crate) use_overrun_detect: bool,
}

pub struct Initialized {
    /// Currently selected debug port. For targets without multidrop,
    /// this will always be the single, default debug port in the system.
    pub(crate) current_dp: DpAddress,
    dps: HashMap<DpAddress, DpState>,
    use_overrun_detect: bool,
    sequence: Arc<dyn ArmDebugSequence>,
}

impl Initialized {
    pub fn new(
        sequence: Arc<dyn ArmDebugSequence>,
        current_dp: DpAddress,
        use_overrun_detect: bool,
    ) -> Self {
        Self {
            current_dp,
            dps: HashMap::new(),
            use_overrun_detect,
            sequence,
        }
    }
}

impl ArmDebugState for Uninitialized {}

impl ArmDebugState for Initialized {
    fn disconnect(&mut self, probe: &mut dyn DapProbe) {
        let stop_span = tracing::debug_span!("debug_port_stop").entered();

        // Stop the current DP, which may not be one of the known ones (i.e. RP2040 rescue DP).
        self.sequence.debug_port_stop(probe, self.current_dp).ok();

        // Stop all intentionally-connected DPs.
        for dp in self.dps.keys().filter(|dp| **dp != self.current_dp) {
            // Try to select the debug port we want to shut down.
            if self.sequence.debug_port_connect(probe, *dp).is_ok() {
                self.sequence.debug_port_stop(probe, *dp).ok();
            } else {
                tracing::warn!("Failed to stop DP {:x?}", dp);
            }
        }
        probe.raw_flush().ok();
        drop(stop_span);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectCache {
    DPv1(SelectV1),
    DPv3(SelectV3, Select1),
}
impl SelectCache {
    pub fn dp_bank_sel(&self) -> u8 {
        match self {
            SelectCache::DPv1(s) => s.dp_bank_sel(),
            SelectCache::DPv3(s, _) => s.dp_bank_sel(),
        }
    }
    pub fn set_dp_bank_sel(&mut self, bank: u8) {
        match self {
            SelectCache::DPv1(s) => s.set_dp_bank_sel(bank),
            SelectCache::DPv3(s, _) => s.set_dp_bank_sel(bank),
        }
    }
}
#[derive(Debug)]
pub(crate) struct DpState {
    pub debug_port_version: DebugPortVersion,

    pub(crate) current_select: SelectCache,
}

impl DpState {
    pub fn new() -> Self {
        Self {
            debug_port_version: DebugPortVersion::Unsupported(0xFF),
            current_select: SelectCache::DPv1(SelectV1(0)),
        }
    }
}

/// An implementation of the communication protocol between probe and target.
/// Can be used to perform all sorts of generic debug access on ARM targets with probes that support low level access.
/// (E.g. CMSIS-DAP and J-Link support this, ST-Link does not)
#[derive(Debug)]
pub struct ArmCommunicationInterface<S: ArmDebugState> {
    probe: Option<Box<dyn DapProbe>>,
    state: S,
}

impl<S: ArmDebugState> Drop for ArmCommunicationInterface<S> {
    fn drop(&mut self) {
        if let Some(mut probe) = self.probe.take() {
            self.state.disconnect(&mut *probe);
        }
    }
}

impl<S: ArmDebugState> ArmCommunicationInterface<S> {
    pub(crate) fn probe_mut(&mut self) -> &mut dyn DapProbe {
        // Unwrap: Probe is only taken when the struct is dropped
        self.probe.as_deref_mut().expect("ArmCommunicationInterface is in an inconsistent state. This is a bug, please report it.")
    }

    fn close(mut self) -> Probe {
        let mut probe = self.probe.take().expect("ArmCommunicationInterface is in an inconsistent state. This is a bug, please report it.");

        self.state.disconnect(&mut *probe);

        Probe::from_attached_probe(RawDapAccess::into_probe(probe))
    }
}

/// Helper trait for probes which offer access to ARM DAP (Debug Access Port).
///
/// This is used to combine the traits, because it cannot be done in the ArmCommunicationInterface
/// struct itself.
pub trait DapProbe: RawDapAccess + DebugProbe {}

impl ArmProbeInterface for ArmCommunicationInterface<Initialized> {
    fn reinitialize(&mut self) -> Result<(), ArmError> {
        // Simulate the drop / close of the initialized communication interface.
        let mut probe = self.probe.take().expect("ArmCommunicationInterface is in an inconsistent state. This is a bug, please report it.");
        self.state.disconnect(&mut *probe);

        match Self::try_setup(
            probe,
            self.state.sequence.clone(),
            self.current_debug_port(),
            self.state.use_overrun_detect,
        ) {
            Ok(reinitialized) => {
                let _ = std::mem::replace(self, reinitialized);
                // Dropping the original self. Since we've taken the probe, we've ensured
                // that the drop effects don't happen again.
                Ok(())
            }
            Err((probe, err)) => {
                self.probe.replace(probe);
                Err(err)
            }
        }
    }

    fn memory_interface(
        &mut self,
        access_port_address: &FullyQualifiedApAddress,
    ) -> Result<Box<dyn ArmMemoryInterface + '_>, ArmError> {
        let memory_interface = match access_port_address.ap() {
            ApAddress::V1(_) => Box::new(ADIMemoryInterface::new(self, access_port_address)?)
                as Box<dyn ArmMemoryInterface + '_>,
            ApAddress::V2(_) => ap::v2::new_memory_interface(self, access_port_address)?,
        };
        Ok(memory_interface)
    }

    fn current_debug_port(&self) -> DpAddress {
        self.state.current_dp
    }

    fn close(self: Box<Self>) -> Probe {
        ArmCommunicationInterface::close(*self)
    }

    fn access_ports(
        &mut self,
        dp: DpAddress,
    ) -> Result<BTreeSet<FullyQualifiedApAddress>, ArmError> {
        match self.select_dp(dp).map(|state| state.debug_port_version)? {
            DebugPortVersion::DPv0 | DebugPortVersion::DPv1 | DebugPortVersion::DPv2 => {
                Ok(ap::v1::valid_access_ports(self, dp).into_iter().collect())
            }
            DebugPortVersion::DPv3 => ap::v2::enumerate_access_ports(self, dp),
            DebugPortVersion::Unsupported(_) => unreachable!(),
        }
    }
}

impl<S: ArmDebugState> SwdSequence for ArmCommunicationInterface<S> {
    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        self.probe_mut().swj_sequence(bit_len, bits)?;

        Ok(())
    }

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        self.probe_mut().swj_pins(pin_out, pin_select, pin_wait)
    }
}

impl ArmCommunicationInterface<Uninitialized> {
    pub(crate) fn new(probe: Box<dyn DapProbe>, use_overrun_detect: bool) -> Self {
        let state = Uninitialized { use_overrun_detect };

        Self {
            probe: Some(probe),
            state,
        }
    }
}

impl UninitializedArmProbe for ArmCommunicationInterface<Uninitialized> {
    fn initialize(
        mut self: Box<Self>,
        sequence: Arc<dyn ArmDebugSequence>,
        dp: DpAddress,
    ) -> Result<Box<dyn ArmProbeInterface>, (Box<dyn UninitializedArmProbe>, Error)> {
        let use_overrun_detect = self.state.use_overrun_detect;
        let probe = self.probe.take().expect("ArmCommunicationInterface is in an inconsistent state. This is a bug, please report it.");

        match ArmCommunicationInterface::<Initialized>::try_setup(
            probe,
            sequence,
            dp,
            use_overrun_detect,
        ) {
            Ok(initialized) => Ok(Box::new(initialized)),
            Err((probe, err)) => Err((
                Box::new(ArmCommunicationInterface::new(probe, use_overrun_detect)),
                Error::Arm(err),
            )),
        }
    }

    fn close(self: Box<Self>) -> Probe {
        ArmCommunicationInterface::close(*self)
    }
}

impl ArmCommunicationInterface<Initialized> {
    /// Set up and start the debug port with brand-new state.
    fn try_setup(
        mut probe: Box<dyn DapProbe>,
        sequence: Arc<dyn ArmDebugSequence>,
        dp: DpAddress,
        use_overrun_detect: bool,
    ) -> Result<Self, (Box<dyn DapProbe>, ArmError)> {
        if let Err(err) = tracing::debug_span!("debug_port_setup")
            .in_scope(|| sequence.debug_port_setup(&mut *probe, dp))
        {
            return Err((probe, err));
        }

        let mut initializing = Self {
            probe: Some(probe),
            state: Initialized::new(sequence, dp, use_overrun_detect),
        };

        if let Err(err) = initializing.select_dp(dp) {
            return Err((initializing.probe.take().unwrap(), err));
        }

        Ok(initializing)
    }

    /// Inform the probe of the [`CoreStatus`] of the chip attached to the probe.
    pub fn core_status_notification(&mut self, state: CoreStatus) {
        self.probe_mut().core_status_notification(state).ok();
    }

    fn select_dp(&mut self, dp: DpAddress) -> Result<&mut DpState, ArmError> {
        let mut switched_dp = false;

        let sequence = self.state.sequence.clone();

        if self.state.current_dp != dp {
            tracing::debug!("Selecting DP {:x?}", dp);

            switched_dp = true;

            self.probe_mut().raw_flush()?;

            // Try to switch to the new DP.
            if let Err(e) = sequence.debug_port_connect(&mut *self.probe_mut(), dp) {
                tracing::warn!("Failed to switch to DP {:x?}: {}", dp, e);

                // Try the more involved debug_port_setup sequence, which also handles dormant mode.
                sequence.debug_port_setup(&mut *self.probe_mut(), dp)?;
            }

            self.state.current_dp = dp;
        }

        // If we don't have  a state for this DP, this means that we haven't run the necessary init sequence yet.
        if let hash_map::Entry::Vacant(entry) = self.state.dps.entry(dp) {
            let sequence = self.state.sequence.clone();

            entry.insert(DpState::new());

            let start_span = tracing::debug_span!("debug_port_start").entered();
            sequence.debug_port_start(self, dp)?;
            drop(start_span);

            // Make sure we enable the overrun detect mode when requested.
            // For "bit-banging" probes, such as JLink or FTDI, we rely on it for good, stable communication.
            // This is required as the default sequence (and most special implementations) does not do this.
            let mut ctrl_reg: Ctrl = self.read_dp_register(dp)?;
            if ctrl_reg.orun_detect() != self.state.use_overrun_detect {
                tracing::debug!("Setting orun_detect: {}", self.state.use_overrun_detect);
                // only write if there’s a need for it.
                ctrl_reg.set_orun_detect(self.state.use_overrun_detect);
                self.write_dp_register(dp, ctrl_reg)?;
            }

            let idr: DebugPortId = self.read_dp_register::<DPIDR>(dp)?.into();
            tracing::info!(
                "Debug Port version: {} MinDP: {:?}",
                idr.version,
                idr.min_dp_support
            );

            let state = self
                .state
                .dps
                .get_mut(&dp)
                .expect("This DP State was inserted earlier in this function");
            state.debug_port_version = idr.version;
            if idr.version == DebugPortVersion::DPv3 {
                state.current_select = SelectCache::DPv3(SelectV3(0), Select1(0));
            }
        } else if switched_dp {
            let sequence = self.state.sequence.clone();

            let start_span = tracing::debug_span!("debug_port_start").entered();
            sequence.debug_port_start(self, dp)?;
            drop(start_span);
        }

        // note(unwrap): Entry gets inserted above
        Ok(self.state.dps.get_mut(&dp).unwrap())
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

    fn select_ap_and_ap_bank(
        &mut self,
        ap: &FullyQualifiedApAddress,
        ap_register_address: u64,
    ) -> Result<(), ArmError> {
        let dp_state = self.select_dp(ap.dp())?;

        let previous_select = dp_state.current_select;
        match (ap.ap(), &mut dp_state.current_select) {
            (ApAddress::V1(port), SelectCache::DPv1(s)) => {
                let ap_register_address = (ap_register_address & 0xFF) as u8;
                let ap_bank = ap_register_address >> 4;
                s.set_ap_sel(*port);
                s.set_ap_bank_sel(ap_bank);
            }
            (ApAddress::V2(base), SelectCache::DPv3(s, s1)) => {
                let address = base.0.unwrap_or(0) + ap_register_address;
                s.set_addr(((address >> 4) & 0xFFFF_FFFF) as u32);
                s1.set_addr((address >> 32) as u32);
            }
            _ => unreachable!(
                "Did not expect to be called with {ap:x?}. This is a bug, please report it."
            ),
        }

        if previous_select != dp_state.current_select {
            tracing::debug!("Changing SELECT to {:x?}", dp_state.current_select);

            match dp_state.current_select {
                SelectCache::DPv1(select) => {
                    self.write_dp_register(ap.dp(), select)?;
                }
                SelectCache::DPv3(select, select1) => {
                    self.write_dp_register(ap.dp(), select)?;
                    self.write_dp_register(ap.dp(), select1)?;
                }
            }
        }

        Ok(())
    }
}

impl FlushableArmAccess for ArmCommunicationInterface<Initialized> {
    fn flush(&mut self) -> Result<(), ArmError> {
        self.probe_mut().raw_flush()
    }
}

impl SwoAccess for ArmCommunicationInterface<Initialized> {
    fn enable_swo(&mut self, config: &SwoConfig) -> Result<(), ArmError> {
        match self.probe_mut().get_swo_interface_mut() {
            Some(interface) => interface.enable_swo(config),
            None => Err(ArmError::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }

    fn disable_swo(&mut self) -> Result<(), ArmError> {
        match self.probe_mut().get_swo_interface_mut() {
            Some(interface) => interface.disable_swo(),
            None => Err(ArmError::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }

    fn read_swo_timeout(&mut self, timeout: Duration) -> Result<Vec<u8>, ArmError> {
        match self.probe_mut().get_swo_interface_mut() {
            Some(interface) => interface.read_swo_timeout(timeout),
            None => Err(ArmError::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }
}

impl DapAccess for ArmCommunicationInterface<Initialized> {
    fn read_raw_dp_register(
        &mut self,
        dp: DpAddress,
        address: DpRegisterAddress,
    ) -> Result<u32, ArmError> {
        self.select_dp_and_dp_bank(dp, &address)?;
        let result = self.probe_mut().raw_read_register(address.into())?;
        Ok(result)
    }

    fn write_raw_dp_register(
        &mut self,
        dp: DpAddress,
        address: DpRegisterAddress,
        value: u32,
    ) -> Result<(), ArmError> {
        self.select_dp_and_dp_bank(dp, &address)?;
        self.probe_mut().raw_write_register(address.into(), value)?;
        Ok(())
    }

    fn read_raw_ap_register(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
    ) -> Result<u32, ArmError> {
        self.select_ap_and_ap_bank(ap, address)?;

        let result = self
            .probe_mut()
            .raw_read_register(RegisterAddress::ApRegister((address & 0xFF) as u8))?;

        Ok(result)
    }

    fn read_raw_ap_register_repeated(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        self.select_ap_and_ap_bank(ap, address)?;

        self.probe_mut()
            .raw_read_block(RegisterAddress::ApRegister((address & 0xFF) as u8), values)?;
        Ok(())
    }

    fn write_raw_ap_register(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
        value: u32,
    ) -> Result<(), ArmError> {
        self.select_ap_and_ap_bank(ap, address)?;

        self.probe_mut()
            .raw_write_register(RegisterAddress::ApRegister((address & 0xFF) as u8), value)?;

        Ok(())
    }

    fn write_raw_ap_register_repeated(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
        values: &[u32],
    ) -> Result<(), ArmError> {
        self.select_ap_and_ap_bank(ap, address)?;

        self.probe_mut()
            .raw_write_block(RegisterAddress::ApRegister((address & 0xFF) as u8), values)?;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), ArmError> {
        self.probe_mut().raw_flush()
    }

    fn try_dap_probe(&self) -> Option<&dyn DapProbe> {
        self.probe.as_deref()
    }

    fn try_dap_probe_mut(&mut self) -> Option<&mut dyn DapProbe> {
        self.probe
            .as_deref_mut()
            // Need to explicitly coerce lifetimes: https://github.com/rust-lang/rust/issues/108999
            .map(|p: &mut (dyn DapProbe + 'static)| p as &mut (dyn DapProbe + '_))
    }
}

/// Information about the chip target we are currently attached to.
/// This can be used for discovery, tho, for now it does not work optimally,
/// as some manufacturers (e.g. ST Microelectronics) violate the spec and thus need special discovery procedures.
#[derive(Debug, Clone, Copy)]
pub struct ArmChipInfo {
    /// The JEP106 code of the manufacturer of this chip target.
    pub manufacturer: JEP106Code,
    /// The unique part number of the chip target. Unfortunately this only unique in the spec.
    /// In practice some manufacturers violate the spec and assign a part number to an entire family.
    ///
    /// Consider this not unique when working with targets!
    pub part: u16,
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
}
