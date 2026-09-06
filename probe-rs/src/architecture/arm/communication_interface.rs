use crate::{
    CoreStatus,
    architecture::arm::{
        ApAddress, ArmError, DapAccess, FullyQualifiedApAddress, RegisterAddress, SwoAccess,
        SwoConfig, ap,
        dp::{
            Ctrl, DPIDR, DebugPortId, DebugPortVersion, DpAccess, DpAddress, DpRegisterAddress,
            Select1, SelectV1, SelectV3,
        },
        memory::{ADIMemoryInterface, ArmMemoryInterface, Component},
        sequences::ArmDebugSequence,
        traits::DebugPortWire,
    },
    probe::{
        BitSequence, DebugProbe, DebugProbeError, JtagChainAccess, Probe, SwdProbe, SwdSettings,
        WireProtocol,
        jtag::dap::{
            jtag_output_sequence, jtag_read_block, jtag_read_register, jtag_write_block,
            jtag_write_register,
        },
        swd::{Port, SwdBatch, SwdOp, SwdPort, SwdPortError, SwdTransferError},
    },
};
use jep106::JEP106Code;

use crate::probe::jtag::chain::JtagChain;
use crate::probe::swd::Pins;

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
    /// A protocol error occurred during communication.
    #[error("A protocol error occurred in the {0} communication between probe and device.")]
    Protocol(WireProtocol),
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

/// To be implemented by debug probe drivers that support the ARM debug interface.
pub trait ArmDebugInterface: DapAccess + SwdSequence + SwoAccess + Send {
    /// Reinitialize the communication interface (in place).
    ///
    /// Some chip-specific reset sequences may disable the debug port. `reinitialize` allows
    /// a debug sequence to re-initialize the debug port.
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

    /// Explicitly select a debug port.
    ///
    /// It is required that implementations connect to a debug port automatically,
    /// but this method can be used to select a DP manually, to have control
    /// over the point in time where the DP is selected.
    fn select_debug_port(&mut self, dp: DpAddress) -> Result<(), ArmError>;

    /// Return the currently connected debug port.
    ///
    /// None if the interface is not connected to a DP.
    fn current_debug_port(&self) -> Option<DpAddress>;

    /// Returns a memory interface to access the target's memory.
    fn memory_interface(
        &mut self,
        access_port: &FullyQualifiedApAddress,
    ) -> Result<Box<dyn ArmMemoryInterface + '_>, ArmError>;

    /// Inform the probe driver of the attached core status.
    fn core_status_notification(&mut self, _state: CoreStatus) {}

    /// Returns the transport protocol in use when known.
    fn active_wire_protocol(&self) -> Option<WireProtocol> {
        None
    }

    /// Returns the current wire speed in kHz when supported.
    fn wire_speed_khz(&self) -> Option<u32> {
        None
    }

    /// Sets the wire speed in kHz when supported.
    fn set_wire_speed(&mut self, _speed_khz: u32) -> Result<u32, ArmError> {
        Err(ArmError::NotImplemented("set_wire_speed"))
    }
}

/// Read chip information from the ROM tables
pub fn read_chip_info_from_rom_table(
    probe: &mut dyn ArmDebugInterface,
    dp: DpAddress,
) -> Result<Option<ArmChipInfo>, ArmError> {
    for ap in probe.access_ports(dp)? {
        if let Ok(mut memory) = probe.memory_interface(&ap) {
            let base_address = memory.base_address()?;
            let component = Component::try_parse(&mut *memory, base_address)?;

            if let Component::Class1RomTable(component_id, _) = component
                && let Some(jep106) = component_id.peripheral_id().jep106()
            {
                return Ok(Some(ArmChipInfo {
                    manufacturer: jep106,
                    part: component_id.peripheral_id().part(),
                }));
            }
        }
    }

    Ok(None)
}

// TODO: Rename trait!
/// Support for sending raw sequences via the probe.
pub trait SwdSequence {
    /// Corresponds to the DAP_SWJ_Sequence function from the ARM Debug sequences
    fn swj_sequence(&mut self, bits: &BitSequence) -> Result<(), DebugProbeError>;

    /// Corresponds to the DAP_SWJ_Pins function from the ARM Debug sequences
    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError>;
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

/// The probe driver behind an [`ArmCommunicationInterface`].
enum ArmProbe {
    /// An SWD probe with its transaction settings.
    Swd(Box<dyn SwdProbe>, SwdSettings),
    /// A JTAG probe with scan chain state.
    Jtag(Box<dyn JtagChainAccess>),
}

impl std::fmt::Debug for ArmProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Swd(_, _) => f.write_str("Swd(..)"),
            Self::Jtag(_) => f.write_str("Jtag(..)"),
        }
    }
}

struct SwdDebugPortWire<'a> {
    probe: &'a mut dyn SwdProbe,
    settings: SwdSettings,
}

impl SwdDebugPortWire<'_> {
    fn with_port<R>(
        &mut self,
        f: impl FnOnce(&mut SwdPort<'_>) -> Result<R, SwdPortError>,
    ) -> Result<R, ArmError> {
        let mut port = SwdPort::new(
            self.probe,
            ArmCommunicationInterface::copy_swd_settings(&self.settings),
        );
        f(&mut port).map_err(ArmCommunicationInterface::swd_port_error)
    }

    fn run_probe_batch(&mut self, batch: &SwdBatch) -> Result<(), ArmError> {
        self.probe
            .run_batch(batch)
            .map_err(|error| ArmError::Probe(batch_probe_error(error)))?;
        Ok(())
    }
}

impl DebugPortWire for SwdDebugPortWire<'_> {
    fn active_protocol(&self) -> Option<WireProtocol> {
        Some(WireProtocol::Swd)
    }

    fn swj_sequence(&mut self, bits: &BitSequence) -> Result<(), ArmError> {
        let mut batch = SwdBatch::new();
        batch.sequence(bits.clone());
        self.run_probe_batch(&batch)
    }

    fn jtag_sequence(&mut self, _tms: bool, _tdi: &BitSequence) -> Result<(), ArmError> {
        Err(ArmError::Probe(
            DebugProbeError::CommandNotSupportedByProbe {
                command_name: "jtag_sequence",
            },
        ))
    }

    fn configure_jtag(&mut self, _skip_scan: bool) -> Result<(), ArmError> {
        Err(ArmError::Probe(
            DebugProbeError::CommandNotSupportedByProbe {
                command_name: "configure_jtag",
            },
        ))
    }

    fn swj_pins(&mut self, out: Pins, select: Pins, wait: Duration) -> Result<Pins, ArmError> {
        let mut batch = SwdBatch::new();
        let _ = batch.schedule(SwdOp::Pins { out, select, wait });
        self.run_probe_batch(&batch)?;
        Ok(Pins(0xFF))
    }

    fn target_reset(&mut self) -> Result<(), ArmError> {
        self.probe.target_reset().map_err(ArmError::Probe)
    }

    fn target_reset_assert(&mut self) -> Result<(), ArmError> {
        self.probe.target_reset_assert().map_err(ArmError::Probe)
    }

    fn target_reset_deassert(&mut self) -> Result<(), ArmError> {
        self.probe.target_reset_deassert().map_err(ArmError::Probe)
    }

    fn raw_flush(&mut self) -> Result<(), ArmError> {
        Ok(())
    }

    fn raw_read_register(&mut self, port: Port, addr: u8) -> Result<u32, ArmError> {
        self.with_port(|swd_port| {
            let mut batch = SwdBatch::new();
            let handle = batch.read(port, addr);
            let mut results = swd_port.run(batch)?;
            Ok(results.take(handle).unwrap())
        })
    }

    fn raw_write_register(&mut self, port: Port, addr: u8, value: u32) -> Result<(), ArmError> {
        self.with_port(|swd_port| {
            let mut batch = SwdBatch::new();
            batch.write(port, addr, value);
            swd_port.run(batch)?;
            Ok(())
        })
    }

    fn try_jtag_chain(&mut self) -> Option<JtagChain<'_>> {
        None
    }
}

struct JtagDebugPortWire<'a>(&'a mut dyn JtagChainAccess);

impl DebugPortWire for JtagDebugPortWire<'_> {
    fn active_protocol(&self) -> Option<WireProtocol> {
        Some(WireProtocol::Jtag)
    }

    fn swj_sequence(&mut self, _bits: &BitSequence) -> Result<(), ArmError> {
        Err(ArmError::Probe(
            DebugProbeError::CommandNotSupportedByProbe {
                command_name: "swj_sequence",
            },
        ))
    }

    fn jtag_sequence(&mut self, tms: bool, tdi: &BitSequence) -> Result<(), ArmError> {
        jtag_output_sequence(self.0, tms, tdi).map_err(ArmError::Probe)
    }

    fn configure_jtag(&mut self, skip_scan: bool) -> Result<(), ArmError> {
        self.0.configure_jtag(skip_scan).map_err(ArmError::Probe)
    }

    fn swj_pins(&mut self, _out: Pins, _select: Pins, _wait: Duration) -> Result<Pins, ArmError> {
        Err(ArmError::Probe(
            DebugProbeError::CommandNotSupportedByProbe {
                command_name: "swj_pins",
            },
        ))
    }

    fn target_reset(&mut self) -> Result<(), ArmError> {
        self.0.target_reset().map_err(ArmError::Probe)
    }

    fn target_reset_assert(&mut self) -> Result<(), ArmError> {
        self.0.target_reset_assert().map_err(ArmError::Probe)
    }

    fn target_reset_deassert(&mut self) -> Result<(), ArmError> {
        self.0.target_reset_deassert().map_err(ArmError::Probe)
    }

    fn raw_flush(&mut self) -> Result<(), ArmError> {
        Ok(())
    }

    fn raw_read_register(&mut self, port: Port, addr: u8) -> Result<u32, ArmError> {
        let register = dp_wire_register(port, addr);
        let mut chain = JtagChain::new(self.0);
        jtag_read_register(&mut chain, register)
    }

    fn raw_write_register(&mut self, port: Port, addr: u8, value: u32) -> Result<(), ArmError> {
        let register = dp_wire_register(port, addr);
        let mut chain = JtagChain::new(self.0);
        jtag_write_register(&mut chain, register, value)
    }

    fn try_jtag_chain(&mut self) -> Option<JtagChain<'_>> {
        Some(JtagChain::new(self.0))
    }
}

fn dp_wire_register(port: Port, addr: u8) -> RegisterAddress {
    match port {
        Port::Dp => RegisterAddress::DpRegister(DpRegisterAddress {
            address: addr,
            bank: None,
        }),
        Port::Ap => RegisterAddress::ApRegister(addr),
    }
}

pub(crate) fn probe_debug_port_wire<R>(
    probe: &mut dyn DebugProbe,
    f: impl FnOnce(&mut dyn DebugPortWire) -> Result<R, ArmError>,
) -> Result<R, ArmError> {
    if probe.active_protocol() == Some(WireProtocol::Jtag)
        && let Some(jtag) = probe.try_as_jtag_chain_access_mut()
    {
        let mut wire = JtagDebugPortWire(jtag);
        return f(&mut wire);
    }
    if let Some(swd) = probe.try_as_swd_probe_mut() {
        let settings = swd.swd_settings();
        let settings = ArmCommunicationInterface::copy_swd_settings(&settings);
        let mut wire = SwdDebugPortWire {
            probe: swd,
            settings,
        };
        return f(&mut wire);
    }
    if let Some(jtag) = probe.try_as_jtag_chain_access_mut() {
        let mut wire = JtagDebugPortWire(jtag);
        return f(&mut wire);
    }
    Err(ArmError::NotImplemented("debug_port_wire"))
}

/// An implementation of the communication protocol between probe and target.
/// Can be used to perform all sorts of generic debug access on ARM targets with probes that support low level access.
/// (E.g. CMSIS-DAP and J-Link support this, ST-Link does not)
#[derive(Debug)]
pub struct ArmCommunicationInterface {
    probe: Option<ArmProbe>,

    /// Currently selected debug port. For targets without multidrop,
    /// this will always be the single, default debug port in the system.
    ///
    /// If this is None, the interface is in an uninitialized state.
    current_dp: Option<DpAddress>,
    dps: HashMap<DpAddress, DpState>,
    use_overrun_detect: bool,
    sequence: Arc<dyn ArmDebugSequence>,
}

impl Drop for ArmCommunicationInterface {
    fn drop(&mut self) {
        if self.probe.is_some() {
            self.disconnect();

            // Ensure we don't disconnect twice
            self.probe = None;
        }
    }
}

impl ArmCommunicationInterface {
    fn close(mut self) -> Probe {
        self.disconnect();

        let probe = self.probe.take().unwrap();
        match probe {
            ArmProbe::Swd(probe, _) => Probe::from_attached_probe(probe.into_probe()),
            ArmProbe::Jtag(probe) => Probe::from_attached_probe(probe.into_probe()),
        }
    }

    /// Disconnect from all debug ports, by calling `debug_port_stop` on all DPs which we
    /// are connected to.
    fn disconnect(&mut self) {
        if self.probe.is_none() {
            return;
        }

        let sequence = self.sequence.clone();

        if let Some(current_dp) = self.current_dp.take() {
            let others: Vec<DpAddress> = self
                .dps
                .keys()
                .copied()
                .filter(|dp| *dp != current_dp)
                .collect();

            let _ = self.with_debug_port_wire(|wire| {
                let _stop_span = tracing::debug_span!("debug_port_stop").entered();

                // Stop the current DP, which may not be one of the known ones (i.e. RP2040 rescue DP).
                sequence.debug_port_stop(&mut *wire, current_dp).ok();

                drop(_stop_span);

                // Stop all intentionally-connected DPs.
                for dp in others {
                    // Try to select the debug port we want to shut down.
                    if sequence.debug_port_connect(&mut *wire, dp).is_ok() {
                        sequence.debug_port_stop(&mut *wire, dp).ok();
                    } else {
                        tracing::warn!("Failed to stop DP {:x?}", dp);
                    }
                }

                Ok(())
            });
        }

        self.dps.clear();
        let _ = self.with_debug_port_wire(|wire| {
            wire.raw_flush().ok();
            Ok(())
        });
    }

    fn swd_addr(address: RegisterAddress) -> u8 {
        address.a2_and_3()
    }

    fn ap_swd_addr(address: u64) -> u8 {
        Self::swd_addr(RegisterAddress::ApRegister((address & 0xFF) as u8))
    }

    fn swd_port_error(error: SwdPortError) -> ArmError {
        match error {
            SwdPortError::Transfer(error) => ArmError::Dap(Self::swd_transfer_to_dap(error)),
            SwdPortError::Probe(error) => ArmError::Probe(error),
        }
    }

    fn swd_transfer_to_dap(error: SwdTransferError) -> DapError {
        match error {
            SwdTransferError::NoAcknowledge => DapError::NoAcknowledge,
            SwdTransferError::WaitResponse => DapError::WaitResponse,
            SwdTransferError::FaultResponse => DapError::FaultResponse,
            SwdTransferError::IncorrectParity => DapError::IncorrectParity,
            SwdTransferError::Protocol => DapError::Protocol(WireProtocol::Swd),
        }
    }

    fn copy_swd_settings(settings: &SwdSettings) -> SwdSettings {
        SwdSettings {
            num_idle_cycles_between_writes: settings.num_idle_cycles_between_writes,
            num_retries_after_wait: settings.num_retries_after_wait,
            max_retry_idle_cycles_after_wait: settings.max_retry_idle_cycles_after_wait,
            idle_cycles_before_write_verify: settings.idle_cycles_before_write_verify,
            idle_cycles_after_transfer: settings.idle_cycles_after_transfer,
        }
    }

    fn with_swd_port<R>(
        &mut self,
        f: impl FnOnce(&mut SwdPort<'_>) -> Result<R, SwdPortError>,
    ) -> Result<R, ArmError> {
        match self.probe.as_mut().unwrap() {
            ArmProbe::Swd(probe, settings) => {
                let copied = Self::copy_swd_settings(settings);
                let mut port = SwdPort::new(probe.as_mut(), copied);
                f(&mut port).map_err(Self::swd_port_error)
            }
            _ => panic!("ArmCommunicationInterface does not hold an SwdProbe"),
        }
    }

    fn with_jtag_chain<R>(
        &mut self,
        f: impl FnOnce(&mut JtagChain<'_>) -> Result<R, ArmError>,
    ) -> Result<R, ArmError> {
        match self.probe.as_mut().unwrap() {
            ArmProbe::Jtag(probe) => {
                let mut chain = JtagChain::new(probe.as_mut());
                f(&mut chain)
            }
            _ => panic!("ArmCommunicationInterface does not hold a JtagChainAccess"),
        }
    }

    fn with_debug_port_wire<R>(
        &mut self,
        f: impl FnOnce(&mut dyn DebugPortWire) -> Result<R, ArmError>,
    ) -> Result<R, ArmError> {
        match self.probe.as_mut().expect(
            "ArmCommunicationInterface is in an inconsistent state. This is a bug, please report it.",
        ) {
            ArmProbe::Swd(probe, settings) => {
                let mut wire = SwdDebugPortWire {
                    probe: probe.as_mut(),
                    settings: Self::copy_swd_settings(settings),
                };
                f(&mut wire)
            }
            ArmProbe::Jtag(probe) => {
                let mut wire = JtagDebugPortWire(probe.as_mut());
                f(&mut wire)
            }
        }
    }
}

impl ArmDebugInterface for ArmCommunicationInterface {
    fn reinitialize(&mut self) -> Result<(), ArmError> {
        let current_dp = self.current_dp;

        // Simulate the drop / close of the initialized communication interface.
        self.disconnect();

        // This should be set to None by the disconnect call above.
        assert!(self.current_dp.is_none());
        assert!(self.dps.is_empty());

        // Reconnect to the DP again
        if let Some(dp) = current_dp {
            self.select_dp(dp)?;
        }

        Ok(())
    }

    fn memory_interface(
        &mut self,
        access_port_address: &FullyQualifiedApAddress,
    ) -> Result<Box<dyn ArmMemoryInterface + '_>, ArmError> {
        let memory_interface: Box<dyn ArmMemoryInterface + '_> = match access_port_address.ap() {
            ApAddress::V1(_) => Box::new(ADIMemoryInterface::new(self, access_port_address)?),
            ApAddress::V2(_) => ap::v2::new_memory_interface(self, access_port_address)?,
        };
        Ok(memory_interface)
    }

    fn current_debug_port(&self) -> Option<DpAddress> {
        self.current_dp
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

    fn select_debug_port(&mut self, dp: DpAddress) -> Result<(), ArmError> {
        let _ = self.select_dp(dp)?;
        Ok(())
    }

    fn core_status_notification(&mut self, state: CoreStatus) {
        if let Some(probe) = self.probe.as_mut() {
            let debug_probe: &mut dyn DebugProbe = match probe {
                ArmProbe::Swd(probe, _) => probe.as_mut(),
                ArmProbe::Jtag(probe) => probe.as_mut(),
            };
            debug_probe.core_status_notification(state).ok();
        }
    }

    fn active_wire_protocol(&self) -> Option<WireProtocol> {
        match self.probe.as_ref()? {
            ArmProbe::Swd(_, _) => Some(WireProtocol::Swd),
            ArmProbe::Jtag(_) => Some(WireProtocol::Jtag),
        }
    }

    fn wire_speed_khz(&self) -> Option<u32> {
        match self.probe.as_ref()? {
            ArmProbe::Swd(probe, _) => Some(probe.speed_khz()),
            ArmProbe::Jtag(probe) => Some(probe.speed_khz()),
        }
    }

    fn set_wire_speed(&mut self, speed_khz: u32) -> Result<u32, ArmError> {
        let probe = self
            .probe
            .as_mut()
            .ok_or(ArmError::NotImplemented("set_wire_speed"))?;
        match probe {
            ArmProbe::Swd(probe, _) => probe.set_speed(speed_khz).map_err(ArmError::Probe),
            ArmProbe::Jtag(probe) => probe.set_speed(speed_khz).map_err(ArmError::Probe),
        }
    }
}

impl SwdSequence for ArmCommunicationInterface {
    fn swj_sequence(&mut self, bits: &BitSequence) -> Result<(), DebugProbeError> {
        match self.probe.as_mut().unwrap() {
            ArmProbe::Swd(probe, _) => {
                let mut batch = SwdBatch::new();
                batch.sequence(bits.clone());
                probe.run_batch(&batch).map_err(batch_probe_error)?;
                Ok(())
            }
            ArmProbe::Jtag(_) => Err(DebugProbeError::CommandNotSupportedByProbe {
                command_name: "swj_sequence",
            }),
        }
    }

    fn swj_pins(
        &mut self,
        _pin_out: u32,
        _pin_select: u32,
        _pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        match self.probe.as_mut().unwrap() {
            ArmProbe::Swd(_, _) | ArmProbe::Jtag(_) => {
                Err(DebugProbeError::CommandNotSupportedByProbe {
                    command_name: "swj_pins",
                })
            }
        }
    }
}

fn batch_probe_error(error: crate::probe::BatchExecutionError<DebugProbeError>) -> DebugProbeError {
    match error.error {
        crate::probe::BatchError::Probe(error) => error,
        crate::probe::BatchError::Specific(error) => error,
    }
}

impl ArmCommunicationInterface {
    /// Create a new SWD interface over a layer-0 probe.
    pub fn create_swd(
        probe: Box<dyn SwdProbe>,
        settings: SwdSettings,
        sequence: Arc<dyn ArmDebugSequence>,
        use_overrun_detect: bool,
    ) -> Box<dyn ArmDebugInterface> {
        let interface = ArmCommunicationInterface {
            probe: Some(ArmProbe::Swd(probe, settings)),
            current_dp: None,
            dps: Default::default(),
            use_overrun_detect,
            sequence,
        };

        Box::new(interface)
    }

    /// Create a new JTAG interface over a layer-0 probe.
    pub fn create_jtag(
        probe: Box<dyn JtagChainAccess>,
        sequence: Arc<dyn ArmDebugSequence>,
        use_overrun_detect: bool,
    ) -> Box<dyn ArmDebugInterface> {
        let interface = ArmCommunicationInterface {
            probe: Some(ArmProbe::Jtag(probe)),
            current_dp: None,
            dps: Default::default(),
            use_overrun_detect,
            sequence,
        };

        Box::new(interface)
    }

    fn select_dp(&mut self, dp: DpAddress) -> Result<&mut DpState, ArmError> {
        let mut switched_dp = false;

        let sequence = self.sequence.clone();

        if self.current_dp != Some(dp) {
            tracing::debug!("Selecting DP {:x?}", dp);

            switched_dp = true;
            let previous_dp = self.current_dp;

            self.with_debug_port_wire(|wire| {
                wire.raw_flush()?;

                if previous_dp.is_none() {
                    sequence.debug_port_setup(wire, dp)?;
                } else if let Err(e) = sequence.debug_port_connect(wire, dp) {
                    tracing::debug!(
                        "Quick connect to DP {:x?} failed ({e}), running full setup",
                        dp
                    );
                    sequence.debug_port_setup(wire, dp)?;
                }

                Ok(())
            })?;

            self.current_dp = Some(dp);
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
            (ApAddress::V1(port), SelectCache::DPv3(s, s1)) if *port == 0 => {
                // Some externally generated target descriptions still model the root APv2
                // memory interface as legacy AP index 0. Treat that as base address 0x0
                // so DPv3/MINDP targets remain usable.
                tracing::warn!(
                    "DPv3 target was configured with legacy AP index 0; treating it as APv2 base 0x0"
                );
                let address = ap_register_address;
                s.set_addr(((address >> 4) & 0xFFFF_FFFF) as u32);
                s1.set_addr((address >> 32) as u32);
            }
            (ApAddress::V1(port), SelectCache::DPv3(_, _)) => {
                return Err(ArmError::Other(format!(
                    "DPv3 targets require APv2 addresses in target descriptions; got legacy AP index {port}. Use `ap: !v2 0x...`."
                )));
            }
            (ApAddress::V2(_), SelectCache::DPv1(_)) => {
                return Err(ArmError::Other(format!(
                    "The selected target uses an APv2 address ({ap:x?}), but the connected debug port only supports the ADIv5 SELECT register. This usually means the wrong chip was selected for the attached target."
                )));
            }
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

impl SwoAccess for ArmCommunicationInterface {
    fn enable_swo(&mut self, config: &SwoConfig) -> Result<(), ArmError> {
        let probe: &mut dyn DebugProbe = match self.probe.as_mut().unwrap() {
            ArmProbe::Swd(probe, _) => probe.as_mut(),
            ArmProbe::Jtag(probe) => probe.as_mut(),
        };
        match probe.get_swo_interface_mut() {
            Some(interface) => interface.enable_swo(config),
            None => Err(ArmError::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }

    fn disable_swo(&mut self) -> Result<(), ArmError> {
        let probe: &mut dyn DebugProbe = match self.probe.as_mut().unwrap() {
            ArmProbe::Swd(probe, _) => probe.as_mut(),
            ArmProbe::Jtag(probe) => probe.as_mut(),
        };
        match probe.get_swo_interface_mut() {
            Some(interface) => interface.disable_swo(),
            None => Err(ArmError::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }

    fn read_swo_timeout(&mut self, timeout: Duration) -> Result<Vec<u8>, ArmError> {
        let probe: &mut dyn DebugProbe = match self.probe.as_mut().unwrap() {
            ArmProbe::Swd(probe, _) => probe.as_mut(),
            ArmProbe::Jtag(probe) => probe.as_mut(),
        };
        match probe.get_swo_interface_mut() {
            Some(interface) => interface.read_swo_timeout(timeout),
            None => Err(ArmError::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }
}

impl DapAccess for ArmCommunicationInterface {
    fn read_raw_dp_register(
        &mut self,
        dp: DpAddress,
        address: DpRegisterAddress,
    ) -> Result<u32, ArmError> {
        self.select_dp_and_dp_bank(dp, &address)?;
        let register = RegisterAddress::DpRegister(address);
        if matches!(self.probe.as_ref(), Some(ArmProbe::Swd(_, _))) {
            let addr = Self::swd_addr(register);
            return self.with_swd_port(|port| {
                let mut batch = SwdBatch::new();
                let handle = batch.read(Port::Dp, addr);
                let mut results = port.run(batch)?;
                Ok(results.take(handle).unwrap())
            });
        }
        self.with_jtag_chain(|chain| jtag_read_register(chain, register))
    }

    fn write_raw_dp_register(
        &mut self,
        dp: DpAddress,
        address: DpRegisterAddress,
        value: u32,
    ) -> Result<(), ArmError> {
        self.select_dp_and_dp_bank(dp, &address)?;
        let register = RegisterAddress::DpRegister(address);
        if matches!(self.probe.as_ref(), Some(ArmProbe::Swd(_, _))) {
            let addr = Self::swd_addr(register);
            return self.with_swd_port(|port| {
                let mut batch = SwdBatch::new();
                batch.write(Port::Dp, addr, value);
                port.run(batch)?;
                Ok(())
            });
        }
        self.with_jtag_chain(|chain| jtag_write_register(chain, register, value))
    }

    fn read_raw_ap_register(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
    ) -> Result<u32, ArmError> {
        self.select_ap_and_ap_bank(ap, address)?;
        let register = RegisterAddress::ApRegister((address & 0xFF) as u8);

        if matches!(self.probe.as_ref(), Some(ArmProbe::Swd(_, _))) {
            let addr = Self::ap_swd_addr(address);
            return self.with_swd_port(|port| {
                let mut batch = SwdBatch::new();
                let handle = port.read_ap_block(&mut batch, addr, 1);
                let mut results = port.run(batch)?;
                Ok(results.take(handle).unwrap()[0])
            });
        }
        self.with_jtag_chain(|chain| jtag_read_register(chain, register))
    }

    fn read_raw_ap_register_repeated(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        self.select_ap_and_ap_bank(ap, address)?;
        let register = RegisterAddress::ApRegister((address & 0xFF) as u8);

        if matches!(self.probe.as_ref(), Some(ArmProbe::Swd(_, _))) {
            let addr = Self::ap_swd_addr(address);
            let count = values.len();
            return self.with_swd_port(|port| {
                let mut batch = SwdBatch::new();
                let handle = port.read_ap_block(&mut batch, addr, count);
                let mut results = port.run(batch)?;
                let read_values = results.take(handle).unwrap();
                values.copy_from_slice(&read_values);
                Ok(())
            });
        }
        self.with_jtag_chain(|chain| jtag_read_block(chain, register, values))
    }

    fn write_raw_ap_register(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
        value: u32,
    ) -> Result<(), ArmError> {
        self.select_ap_and_ap_bank(ap, address)?;
        let register = RegisterAddress::ApRegister((address & 0xFF) as u8);

        if matches!(self.probe.as_ref(), Some(ArmProbe::Swd(_, _))) {
            let addr = Self::ap_swd_addr(address);
            return self.with_swd_port(|port| {
                let mut batch = SwdBatch::new();
                batch.write(Port::Ap, addr, value);
                port.run(batch)?;
                Ok(())
            });
        }
        self.with_jtag_chain(|chain| jtag_write_register(chain, register, value))
    }

    fn write_raw_ap_register_repeated(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
        values: &[u32],
    ) -> Result<(), ArmError> {
        self.select_ap_and_ap_bank(ap, address)?;
        let register = RegisterAddress::ApRegister((address & 0xFF) as u8);

        if matches!(self.probe.as_ref(), Some(ArmProbe::Swd(_, _))) {
            let addr = Self::ap_swd_addr(address);
            return self.with_swd_port(|port| {
                let mut batch = SwdBatch::new();
                port.write_ap_block(&mut batch, addr, values);
                port.run(batch)?;
                Ok(())
            });
        }
        self.with_jtag_chain(|chain| jtag_write_block(chain, register, values))
    }

    fn flush(&mut self) -> Result<(), ArmError> {
        Ok(())
    }

    fn active_wire_protocol(&self) -> Option<WireProtocol> {
        match self.probe.as_ref()? {
            ArmProbe::Swd(_, _) => Some(WireProtocol::Swd),
            ArmProbe::Jtag(_) => Some(WireProtocol::Jtag),
        }
    }

    fn debug_port_reconnect_with(
        &mut self,
        connect: &mut dyn FnMut(&mut dyn DebugPortWire) -> Result<(), ArmError>,
    ) -> Result<(), ArmError> {
        self.with_debug_port_wire(|wire| connect(wire))
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::architecture::arm::sequences::DefaultArmSequence;
    use crate::probe::BitSequence;
    use crate::probe::swd::mock::{MockSwdProbe, RecordedOp, RecordedSequence};
    use crate::probe::swd::{Direction, Port};

    const DP_RDBUFF_ADDR: u8 = 0b1100;
    const DP_SELECT_ADDR: u8 = 0b1000;
    const DP_CTRL_ADDR: u8 = 0b0100;

    fn swd_interface(
        probe: MockSwdProbe,
    ) -> (
        Box<dyn ArmDebugInterface>,
        Arc<std::sync::Mutex<Vec<RecordedOp>>>,
    ) {
        let operations = probe.shared_operations();
        let interface = ArmCommunicationInterface::create_swd(
            Box::new(probe),
            SwdSettings::default(),
            DefaultArmSequence::create(),
            false,
        );
        (interface, operations)
    }

    fn read_ops(ops: &[RecordedOp]) -> Vec<(Port, u8)> {
        ops.iter()
            .filter(|op| op.direction == Direction::Read)
            .map(|op| (op.port, op.addr))
            .collect()
    }

    fn write_ops(ops: &[RecordedOp]) -> Vec<(Port, u8, u32)> {
        ops.iter()
            .filter(|op| op.direction == Direction::Write)
            .map(|op| (op.port, op.addr, op.data))
            .collect()
    }

    fn attach_ctrl_reads(probe: &mut MockSwdProbe, trailing: u32) {
        probe.set_read_sequence(
            Port::Dp,
            DP_CTRL_ADDR,
            std::iter::repeat_n(0xA000_0000, 3)
                .chain(std::iter::repeat_n(trailing, 100))
                .collect(),
        );
    }

    fn finish_attach(
        interface: &mut Box<dyn ArmDebugInterface>,
        operations: &Arc<std::sync::Mutex<Vec<RecordedOp>>>,
    ) {
        interface
            .select_debug_port(DpAddress::Default)
            .expect("attach should succeed");
        operations.lock().unwrap().clear();
    }

    fn prepare_swd_probe(probe: &mut MockSwdProbe) {
        push_attach_responses(probe);
        attach_ctrl_reads(probe, 0);
    }

    #[test]
    fn swd_dp_read_writes_select_for_bank_switch() {
        let mut probe = MockSwdProbe::new();
        prepare_swd_probe(&mut probe);
        let (mut interface, operations) = swd_interface(probe);
        finish_attach(&mut interface, &operations);

        let value = interface
            .read_raw_dp_register(
                DpAddress::Default,
                DpRegisterAddress {
                    address: DP_CTRL_ADDR,
                    bank: Some(1),
                },
            )
            .expect("read should succeed");
        assert_eq!(value, 0);

        let ops = operations.lock().unwrap();
        assert_eq!(write_ops(&ops), vec![(Port::Dp, DP_SELECT_ADDR, 1)]);
        assert_eq!(
            read_ops(&ops),
            vec![(Port::Dp, DP_RDBUFF_ADDR), (Port::Dp, DP_CTRL_ADDR)]
        );
    }

    #[test]
    fn swd_dp_write_writes_select_for_bank_switch() {
        let mut probe = MockSwdProbe::new();
        prepare_swd_probe(&mut probe);
        let (mut interface, operations) = swd_interface(probe);
        finish_attach(&mut interface, &operations);

        interface
            .write_raw_dp_register(
                DpAddress::Default,
                DpRegisterAddress {
                    address: DP_CTRL_ADDR,
                    bank: Some(1),
                },
                0x1234,
            )
            .expect("write should succeed");

        let ops = operations.lock().unwrap();
        assert_eq!(
            write_ops(&ops),
            vec![
                (Port::Dp, DP_SELECT_ADDR, 1),
                (Port::Dp, DP_CTRL_ADDR, 0x1234),
            ]
        );
    }

    #[test]
    fn swd_ap_read_includes_rdbuff() {
        let mut probe = MockSwdProbe::new();
        prepare_swd_probe(&mut probe);
        probe.set_read_sequence(Port::Ap, 0b1100, vec![99]);
        probe.set_read_value(Port::Dp, DP_RDBUFF_ADDR, 42);
        let (mut interface, operations) = swd_interface(probe);
        finish_attach(&mut interface, &operations);

        let ap = FullyQualifiedApAddress::v1_with_default_dp(0);
        let value = interface
            .read_raw_ap_register(&ap, 0x0C)
            .expect("read should succeed");
        assert_eq!(value, 42);

        let ops = operations.lock().unwrap();
        assert_eq!(
            read_ops(&ops),
            vec![(Port::Ap, 0b1100), (Port::Dp, DP_RDBUFF_ADDR)]
        );
    }

    #[test]
    fn swd_ap_read_repeated_returns_16_values() {
        let mut probe = MockSwdProbe::new();
        prepare_swd_probe(&mut probe);
        probe.set_read_sequence(Port::Ap, 0b1100, std::iter::once(0).chain(1..=15).collect());
        probe.set_read_value(Port::Dp, DP_RDBUFF_ADDR, 16);
        let (mut interface, operations) = swd_interface(probe);
        finish_attach(&mut interface, &operations);

        let ap = FullyQualifiedApAddress::v1_with_default_dp(0);
        let mut values = vec![0; 16];
        interface
            .read_raw_ap_register_repeated(&ap, 0x0C, &mut values)
            .expect("read should succeed");
        assert_eq!(values, (1..=16).collect::<Vec<_>>());

        let ops = operations.lock().unwrap();
        assert_eq!(read_ops(&ops).len(), 17);
    }

    #[test]
    fn swd_ap_write_repeated_posts_16_values() {
        let mut probe = MockSwdProbe::new();
        prepare_swd_probe(&mut probe);
        let (mut interface, operations) = swd_interface(probe);
        finish_attach(&mut interface, &operations);

        let ap = FullyQualifiedApAddress::v1_with_default_dp(0);
        let values: Vec<u32> = (0..16).map(|i| 0xABCD_0000 | i as u32).collect();
        interface
            .write_raw_ap_register_repeated(&ap, 0x0C, &values)
            .expect("write should succeed");

        let ops = operations.lock().unwrap();
        assert_eq!(
            ops.iter()
                .filter(|op| op.direction == Direction::Write && op.port == Port::Ap)
                .count(),
            16
        );
        assert_eq!(read_ops(&ops).last(), Some(&(Port::Dp, DP_RDBUFF_ADDR)));
    }

    type SwdTestInterface = (
        Box<dyn ArmDebugInterface>,
        Arc<std::sync::Mutex<Vec<RecordedOp>>>,
        Arc<std::sync::Mutex<Vec<RecordedSequence>>>,
    );

    fn swd_interface_with_overrun(
        probe: MockSwdProbe,
        use_overrun_detect: bool,
    ) -> SwdTestInterface {
        let operations = probe.shared_operations();
        let sequences = probe.shared_sequences();
        let interface = ArmCommunicationInterface::create_swd(
            Box::new(probe),
            SwdSettings::default(),
            DefaultArmSequence::create(),
            use_overrun_detect,
        );
        (interface, operations, sequences)
    }

    fn sequence_contains(sequences: &[RecordedSequence], expected: BitSequence) -> bool {
        sequences.iter().any(|recorded| recorded.bits == expected)
    }

    fn push_attach_responses(probe: &mut MockSwdProbe) {
        const DPIDR: u32 = 0x2BA0_1477;

        probe.set_read_value(Port::Dp, 0, DPIDR);
        probe.set_read_value(Port::Dp, DP_RDBUFF_ADDR, 0);
    }

    fn push_multidrop_attach_responses(probe: &mut MockSwdProbe, targetsel: u32) {
        push_attach_responses(probe);
        probe.set_read_sequence(
            Port::Dp,
            DP_CTRL_ADDR,
            [targetsel, targetsel, 0xA000_0000, 0xA000_0000, 0xA000_0000]
                .into_iter()
                .collect(),
        );
    }

    #[test]
    fn swd_select_dp_runs_port_setup_with_jtag_to_swd_switch() {
        let mut probe = MockSwdProbe::new();
        prepare_swd_probe(&mut probe);
        let (mut interface, _, sequences) = swd_interface_with_overrun(probe, false);

        interface
            .select_debug_port(DpAddress::Default)
            .expect("select should succeed");

        let sequences = sequences.lock().unwrap();
        assert!(sequence_contains(
            &sequences,
            BitSequence::from_u64(16, 0xE79E),
        ));
    }

    #[test]
    fn swd_multidrop_select_dp_runs_dormant_entry() {
        let mut probe = MockSwdProbe::new();
        push_multidrop_attach_responses(&mut probe, 0x1234_5678);
        let (mut interface, _, sequences) = swd_interface_with_overrun(probe, false);

        interface
            .select_debug_port(DpAddress::Multidrop(0x1234_5678))
            .expect("select should succeed");

        let sequences = sequences.lock().unwrap();
        assert!(sequence_contains(
            &sequences,
            BitSequence::from_u64(31, 0x33BB_BBBA),
        ));
    }

    #[test]
    fn swd_select_dp_reads_dpidr_and_sets_orun_detect() {
        let mut probe = MockSwdProbe::new();
        prepare_swd_probe(&mut probe);
        let (mut interface, operations, _) = swd_interface_with_overrun(probe, true);

        interface
            .select_debug_port(DpAddress::Default)
            .expect("select should succeed");

        let ops = operations.lock().unwrap();
        assert!(
            ops.iter()
                .any(|op| op.port == Port::Dp && op.addr == 0 && op.direction == Direction::Read),
            "expected a DPIDR read during attach"
        );
        assert!(
            ops.iter().any(|op| {
                op.port == Port::Dp
                    && op.addr == DP_CTRL_ADDR
                    && op.direction == Direction::Write
                    && (op.data & 1) == 1
            }),
            "expected overrun detect to be enabled in CTRL/STAT"
        );
    }
}
