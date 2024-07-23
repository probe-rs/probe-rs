use crate::{
    architecture::{
        arm::{
            communication_interface::ArmProbeInterface,
            component::{get_arm_components, TraceSink},
            memory::CoresightComponent,
            sequences::{ArmDebugSequence, DefaultArmSequence},
            ArmError, DpAddress, SwoReader,
        },
        riscv::communication_interface::{
            RiscvCommunicationInterface, RiscvDebugInterfaceState, RiscvError,
        },
        xtensa::communication_interface::{
            XtensaCommunicationInterface, XtensaDebugInterfaceState, XtensaError,
        },
    },
    config::{CoreExt, DebugSequence, RegistryError, Target, TargetSelector},
    core::{Architecture, CombinedCoreState},
    probe::{
        fake_probe::FakeProbe, list::Lister, AttachMethod, DebugProbeError, Probe,
        ProbeCreationError,
    },
    Core, CoreType, Error,
};
use std::ops::DerefMut;
use std::{fmt, sync::Arc, time::Duration};

/// The `Session` struct represents an active debug session.
///
/// ## Creating a session
/// The session can be created by calling the [Session::auto_attach()] function,
/// which tries to automatically select a probe, and then connect to the target.
///
/// For more control, the [Probe::attach()] and [Probe::attach_under_reset()]
/// methods can be used to open a `Session` from a specific [Probe].
///
/// # Usage
/// The Session is the common handle that gives a user exclusive access to an active probe.
/// You can create and share a session between threads to enable multiple stakeholders (e.g. GDB and RTT) to access the target taking turns, by using `Arc<FairMutex<Session>>`.
///
/// If you do so, make sure that both threads sleep in between tasks such that other stakeholders may take their turn.
///
/// To get access to a single [Core] from the `Session`, the [Session::core()] method can be used.
/// Please see the [Session::core()] method for more usage guidelines.
///
#[derive(Debug)]
pub struct Session {
    target: Target,
    interfaces: ArchitectureInterface,
    cores: Vec<CombinedCoreState>,
    configured_trace_sink: Option<TraceSink>,
}

#[allow(clippy::large_enum_variant)]
enum JtagInterface {
    Riscv(RiscvDebugInterfaceState),
    Xtensa(XtensaDebugInterfaceState),
    Unknown,
}

impl JtagInterface {
    /// Returns the debug module's intended architecture.
    fn architecture(&self) -> Option<Architecture> {
        match self {
            JtagInterface::Riscv(_) => Some(Architecture::Riscv),
            JtagInterface::Xtensa(_) => Some(Architecture::Xtensa),
            JtagInterface::Unknown => None,
        }
    }
}

impl fmt::Debug for JtagInterface {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            JtagInterface::Riscv(_) => f.write_str("Riscv(..)"),
            JtagInterface::Xtensa(_) => f.write_str("Xtensa(..)"),
            JtagInterface::Unknown => f.write_str("Unknown"),
        }
    }
}

// TODO: this is somewhat messy because I omitted separating the Probe out of the ARM interface.
#[allow(clippy::large_enum_variant)]
enum ArchitectureInterface {
    Arm(Box<dyn ArmProbeInterface + 'static>),
    Jtag(Probe, Vec<JtagInterface>),
}

impl fmt::Debug for ArchitectureInterface {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ArchitectureInterface::Arm(_) => f.write_str("ArchitectureInterface::Arm(..)"),
            ArchitectureInterface::Jtag(_, ifaces) => f
                .debug_tuple("ArchitectureInterface::Other(..)")
                .field(ifaces)
                .finish(),
        }
    }
}

impl ArchitectureInterface {
    fn attach<'probe, 'target: 'probe>(
        &'probe mut self,
        target: &'probe Target,
        combined_state: &'probe mut CombinedCoreState,
    ) -> Result<Core<'probe>, Error> {
        match self {
            ArchitectureInterface::Arm(interface) => combined_state.attach_arm(target, interface),
            ArchitectureInterface::Jtag(probe, ifaces) => {
                let idx = combined_state.interface_idx();
                match &mut ifaces[idx] {
                    JtagInterface::Riscv(state) => {
                        let factory = probe.try_get_riscv_interface_builder()?;
                        let iface = factory.attach(state)?;
                        combined_state.attach_riscv(target, iface)
                    }
                    JtagInterface::Xtensa(state) => {
                        let iface = probe.try_get_xtensa_interface(state)?;
                        combined_state.attach_xtensa(target, iface)
                    }
                    JtagInterface::Unknown => {
                        unreachable!(
                            "Tried to attach to unknown interface {idx}. This should never happen."
                        )
                    }
                }
            }
        }
    }
}

impl Session {
    /// Open a new session with a given debug target.
    pub(crate) fn new(
        probe: Probe,
        target: TargetSelector,
        attach_method: AttachMethod,
        permissions: Permissions,
    ) -> Result<Self, Error> {
        let (probe, target) = get_target_from_selector(target, attach_method, probe)?;

        let cores = target
            .cores
            .iter()
            .enumerate()
            .map(|(id, core)| {
                Core::create_state(
                    id,
                    core.core_access_options.clone(),
                    &target,
                    core.core_type,
                )
            })
            .collect();

        let mut session = if let Architecture::Arm = target.architecture() {
            Self::attach_arm(probe, target, attach_method, permissions, cores)?
        } else {
            Self::attach_jtag(probe, target, attach_method, permissions, cores)?
        };

        session.clear_all_hw_breakpoints()?;

        Ok(session)
    }

    fn attach_arm(
        mut probe: Probe,
        target: Target,
        attach_method: AttachMethod,
        permissions: Permissions,
        cores: Vec<CombinedCoreState>,
    ) -> Result<Self, Error> {
        let default_core = target.default_core();

        let default_memory_ap = default_core.memory_ap().ok_or_else(|| {
            Error::Other(format!(
                "Unable to connect to core {default_core:?}, no memory AP configured"
            ))
        })?;

        let default_dp = default_memory_ap.dp();

        let sequence_handle = match &target.debug_sequence {
            DebugSequence::Arm(sequence) => sequence.clone(),
            _ => unreachable!("Mismatch between architecture and sequence type!"),
        };

        if AttachMethod::UnderReset == attach_method {
            let _span = tracing::debug_span!("Asserting hardware reset").entered();

            if let Some(dap_probe) = probe.try_as_dap_probe() {
                sequence_handle.reset_hardware_assert(dap_probe)?;
            } else {
                tracing::info!(
                    "Custom reset sequences are not supported on {}.",
                    probe.get_name()
                );
                tracing::info!("Falling back to standard probe reset.");
                probe.target_reset_assert()?;
            }
        }

        if let Some(jtag) = target.jtag.as_ref() {
            if let Some(scan_chain) = jtag.scan_chain.clone() {
                probe.set_scan_chain(scan_chain)?;
            }
        }
        probe.attach_to_unspecified()?;

        let interface = probe.try_into_arm_interface().map_err(|(_, err)| err)?;

        let mut interface = interface
            .initialize(sequence_handle.clone(), default_dp)
            .map_err(|(_interface, e)| e)?;
        let unlock_span = tracing::debug_span!("debug_device_unlock").entered();

        // Enable debug mode
        let unlock_res =
            sequence_handle.debug_device_unlock(&mut *interface, &default_memory_ap, &permissions);
        drop(unlock_span);

        match unlock_res {
            Ok(()) => (),
            // In case this happens after unlock. Try to re-attach the probe once.
            Err(ArmError::ReAttachRequired) => {
                Self::reattach_arm_interface(&mut interface, &sequence_handle)?;
            }
            Err(e) => return Err(Error::Arm(e)),
        }

        // For each core, setup debugging
        for core in &cores {
            core.enable_arm_debug(&mut *interface)?;
        }

        if attach_method == AttachMethod::UnderReset {
            {
                for core in &cores {
                    core.arm_reset_catch_set(&mut *interface)?;
                }

                let reset_hardware_deassert =
                    tracing::debug_span!("reset_hardware_deassert").entered();

                let mut memory_interface = interface.memory_interface(&default_memory_ap)?;

                // A timeout here indicates that the reset pin is probably not properly connected.
                if let Err(e) = sequence_handle.reset_hardware_deassert(&mut *memory_interface) {
                    if matches!(e, ArmError::Timeout) {
                        tracing::warn!("Timeout while deasserting hardware reset pin. This indicates that the reset pin is not properly connected. Please check your hardware setup.");
                    }

                    return Err(e.into());
                }
                drop(reset_hardware_deassert);
            }

            let mut session = Session {
                target,
                interfaces: ArchitectureInterface::Arm(interface),
                cores,
                configured_trace_sink: None,
            };

            {
                // Wait for the core to be halted. The core should be
                // halted because we set the `reset_catch` earlier, which
                // means that the core should stop when coming out of reset.

                for core_id in 0..session.cores.len() {
                    let mut core = session.core(core_id)?;

                    core.wait_for_core_halted(Duration::from_millis(100))?;

                    core.reset_catch_clear()?;
                }
            }

            Ok(session)
        } else {
            Ok(Session {
                target,
                interfaces: ArchitectureInterface::Arm(interface),
                cores,
                configured_trace_sink: None,
            })
        }
    }

    fn attach_jtag(
        mut probe: Probe,
        target: Target,
        _attach_method: AttachMethod,
        _permissions: Permissions,
        cores: Vec<CombinedCoreState>,
    ) -> Result<Self, Error> {
        // While we still don't support mixed architectures
        // (they'd need per-core debug sequences), we can at least
        // handle most of the setup in the same way.
        if let Some(jtag) = target.jtag.as_ref() {
            if let Some(scan_chain) = jtag.scan_chain.clone() {
                probe.set_scan_chain(scan_chain)?;
            }
        }

        probe.attach_to_unspecified()?;

        // We try to guess the TAP number. Normally we trust the scan chain, but some probes are
        // only quasi-JTAG (wch-link), so we'll have to work with at least 1, but if we're guessing
        // we can also use the highest number specified in the target YAML.

        // FIXME: This is terribly JTAG-specific. Since we don't really support anything else yet,
        // it should be fine for now.
        let highest_idx = cores.iter().map(|c| c.interface_idx()).max().unwrap_or(0);
        let tap_count = match probe.scan_chain() {
            Ok(scan_chain) => scan_chain.len().max(highest_idx + 1),
            Err(_) => highest_idx + 1,
        };
        let mut interfaces = std::iter::repeat_with(|| JtagInterface::Unknown)
            .take(tap_count)
            .collect::<Vec<_>>();

        // Create a new interface by walking through the cores and initialising the TAPs that
        // we find mentioned.
        for core in cores.iter() {
            let iface_idx = core.interface_idx();

            let core_arch = core.core_type().architecture();

            if let Some(debug_arch) = interfaces[iface_idx].architecture() {
                if core_arch == debug_arch {
                    // Already initialised.
                    continue;
                }
                return Err(Error::Probe(DebugProbeError::Other(format!(
                    "{core_arch:?} core can not be mixed with a {debug_arch:?} debug module.",
                ))));
            }

            probe.select_jtag_tap(iface_idx)?;

            interfaces[iface_idx] = match core_arch {
                Architecture::Riscv => {
                    let factory = probe.try_get_riscv_interface_builder()?;
                    let mut state = factory.create_state();
                    {
                        let mut interface = factory.attach(&mut state)?;
                        interface.enter_debug_mode()?;
                    }

                    JtagInterface::Riscv(state)
                }
                Architecture::Xtensa => {
                    let mut state = XtensaDebugInterfaceState::default();
                    {
                        let mut interface = probe.try_get_xtensa_interface(&mut state)?;
                        interface.enter_debug_mode()?;
                    }

                    JtagInterface::Xtensa(state)
                }
                _ => {
                    return Err(Error::Probe(DebugProbeError::Other(format!(
                        "Unsupported core architecture {core_arch:?}",
                    ))));
                }
            };
        }

        let interfaces = ArchitectureInterface::Jtag(probe, interfaces);

        let mut session = Session {
            target,
            interfaces,
            cores,
            configured_trace_sink: None,
        };

        // Wait for the cores to be halted.
        for core_id in 0..session.cores.len() {
            let mut core = session.core(core_id)?;

            core.halt(Duration::from_millis(100))?;
        }

        // Connect to the cores
        match session.target.debug_sequence.clone() {
            DebugSequence::Xtensa(sequence) => {
                for core_id in 0..session.cores.len() {
                    sequence.on_connect(&mut session.get_xtensa_interface(core_id)?)?;
                }
            }

            DebugSequence::Riscv(sequence) => {
                for core_id in 0..session.cores.len() {
                    sequence.on_connect(&mut session.get_riscv_interface(core_id)?)?;
                }
            }
            _ => unreachable!("Other architectures should have already been handled"),
        };

        Ok(session)
    }

    /// Automatically creates a session with the first connected probe found.
    #[tracing::instrument(skip(target))]
    pub fn auto_attach(
        target: impl Into<TargetSelector>,
        permissions: Permissions,
    ) -> Result<Session, Error> {
        // Get a list of all available debug probes.
        let lister = Lister::new();

        let probes = lister.list_all();

        // Use the first probe found.
        let probe = probes
            .first()
            .ok_or(Error::Probe(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::NotFound,
            )))?
            .open()?;

        // Attach to a chip.
        probe.attach(target, permissions)
    }

    /// Lists the available cores with their number and their type.
    pub fn list_cores(&self) -> Vec<(usize, CoreType)> {
        self.cores.iter().map(|t| (t.id(), t.core_type())).collect()
    }

    /// Get access to the session when all cores are halted.
    ///
    /// Any previously running cores will be resumed once the closure is executed.
    pub(crate) fn halted_access<R>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let mut resume_state = vec![];
        for (core, _) in self.list_cores() {
            let mut c = self.core(core)?;
            let status = c.status()?;
            tracing::info!("Core status: {:?}", status);
            if !status.is_halted() {
                tracing::info!("Halting core...");
                resume_state.push(core);
                c.halt(Duration::from_millis(100))?;
            }
        }

        let r = f(self);

        for core in resume_state {
            tracing::info!("Resuming core...");
            self.core(core)?.run()?;
        }

        r
    }

    fn interface_idx(&self, core: usize) -> Result<usize, Error> {
        self.cores
            .get(core)
            .map(|c| c.interface_idx())
            .ok_or(Error::CoreNotFound(core))
    }

    /// Attaches to the core with the given number.
    ///
    /// ## Usage
    /// Every time you want to perform an operation on the chip, you need to get the Core handle with the [Session::core()] method. This [Core] handle is merely a view into the core and provides a convenient API surface.
    ///
    /// All the state is stored in the [Session] handle.
    ///
    /// The first time you call [Session::core()] for a specific core, it will run the attach/init sequences and return a handle to the [Core].
    ///
    /// Every subsequent call is a no-op. It simply returns the handle for the user to use in further operations without calling any int sequences again.
    ///
    /// It is strongly advised to never store the [Core] handle for any significant duration! Free it as fast as possible such that other stakeholders can have access to the [Core] too.
    ///
    /// The idea behind this is: You need the smallest common denominator which you can share between threads. Since you sometimes need the [Core], sometimes the [Probe] or sometimes the [Target], the [Session] is the only common ground and the only handle you should actively store in your code.
    ///
    // By design, this is called frequently in a session, therefore we limit tracing level to "trace" to avoid spamming the logs.
    #[tracing::instrument(level = "trace", skip(self), name = "attach_to_core")]
    pub fn core(&mut self, core_index: usize) -> Result<Core<'_>, Error> {
        let combined_state = self
            .cores
            .get_mut(core_index)
            .ok_or(Error::CoreNotFound(core_index))?;

        self.interfaces.attach(&self.target, combined_state)
    }

    /// Read available trace data from the specified data sink.
    ///
    /// This method is only supported for ARM-based targets, and will
    /// return [ArmError::ArchitectureRequired] otherwise.
    #[tracing::instrument(skip(self))]
    pub fn read_trace_data(&mut self) -> Result<Vec<u8>, ArmError> {
        let sink = self
            .configured_trace_sink
            .as_ref()
            .ok_or(ArmError::TracingUnconfigured)?;

        match sink {
            TraceSink::Swo(_) => {
                let interface = self.get_arm_interface()?;
                interface.read_swo()
            }

            TraceSink::Tpiu(_) => {
                panic!("Probe-rs does not yet support reading parallel trace ports");
            }

            TraceSink::TraceMemory => {
                let components = self.get_arm_components(DpAddress::Default)?;
                let interface = self.get_arm_interface()?;
                crate::architecture::arm::component::read_trace_memory(interface, &components)
            }
        }
    }

    /// Returns an implementation of [std::io::Read] that wraps [SwoAccess::read_swo].
    ///
    /// The implementation buffers all available bytes from
    /// [SwoAccess::read_swo] on each [std::io::Read::read],
    /// minimizing the chance of a target-side overflow event on which
    /// trace packets are lost.
    ///
    /// [SwoAccess::read_swo]: crate::architecture::arm::swo::SwoAccess
    pub fn swo_reader(&mut self) -> Result<SwoReader, Error> {
        let interface = self.get_arm_interface()?;
        Ok(SwoReader::new(interface))
    }

    /// Get the Arm probe interface.
    pub fn get_arm_interface(&mut self) -> Result<&mut dyn ArmProbeInterface, ArmError> {
        let interface = match &mut self.interfaces {
            ArchitectureInterface::Arm(state) => state.deref_mut(),
            _ => return Err(ArmError::NoArmTarget),
        };

        Ok(interface)
    }

    /// Get the RISC-V probe interface.
    pub fn get_riscv_interface(
        &mut self,
        core_id: usize,
    ) -> Result<RiscvCommunicationInterface, Error> {
        let tap_idx = self.interface_idx(core_id)?;
        if let ArchitectureInterface::Jtag(probe, ifaces) = &mut self.interfaces {
            if let JtagInterface::Riscv(state) = &mut ifaces[tap_idx] {
                let factory = probe.try_get_riscv_interface_builder()?;
                return Ok(factory.attach(state)?);
            }
        }
        Err(RiscvError::NoRiscvTarget.into())
    }

    /// Get the Xtensa probe interface.
    pub fn get_xtensa_interface(
        &mut self,
        core_id: usize,
    ) -> Result<XtensaCommunicationInterface, Error> {
        let tap_idx = self.interface_idx(core_id)?;
        if let ArchitectureInterface::Jtag(probe, ifaces) = &mut self.interfaces {
            if let JtagInterface::Xtensa(state) = &mut ifaces[tap_idx] {
                return Ok(probe.try_get_xtensa_interface(state)?);
            }
        }
        Err(XtensaError::NoXtensaTarget.into())
    }

    #[tracing::instrument(skip_all)]
    fn reattach_arm_interface(
        interface: &mut Box<dyn ArmProbeInterface>,
        debug_sequence: &Arc<dyn ArmDebugSequence>,
    ) -> Result<(), Error> {
        use crate::probe::DebugProbe;

        let current_dp = interface.current_debug_port();

        // In order to re-attach we need an owned instance to the interface
        // but we only have &mut. We can work around that by first creating
        // an instance of a Dummy and then swapping it out for the real one.
        // perform the re-attach and then swap it back.
        let tmp_interface = Box::<FakeProbe>::default().try_get_arm_interface().unwrap();
        let mut tmp_interface = tmp_interface
            .initialize(DefaultArmSequence::create(), DpAddress::Default)
            .unwrap();

        std::mem::swap(interface, &mut tmp_interface);

        tracing::debug!("Re-attaching Probe");
        let mut probe = tmp_interface.close();
        probe.detach()?;
        probe.attach_to_unspecified()?;

        let new_interface = probe.try_into_arm_interface().map_err(|(_, err)| err)?;

        tmp_interface = new_interface
            .initialize(debug_sequence.clone(), current_dp)
            .map_err(|(_interface, e)| e)?;
        // swap it back
        std::mem::swap(interface, &mut tmp_interface);

        tracing::debug!("Probe re-attached");
        Ok(())
    }

    /// Check if the connected device has a debug erase sequence defined
    pub fn has_sequence_erase_all(&self) -> bool {
        match &self.target.debug_sequence {
            DebugSequence::Arm(seq) => seq.debug_erase_sequence().is_some(),
            // Currently, debug_erase_sequence is ARM (and ATSAM) specific
            _ => false,
        }
    }

    /// Erase all flash memory using the Device's Debug Erase Sequence if any
    ///
    /// # Returns
    /// Ok(()) if the device provides a custom erase sequence and it succeeded.
    ///
    /// # Errors
    /// NotImplemented if no custom erase sequence exists
    /// Err(e) if the custom erase sequence failed
    pub fn sequence_erase_all(&mut self) -> Result<(), Error> {
        let ArchitectureInterface::Arm(ref mut interface) = self.interfaces else {
            return Err(Error::NotImplemented(
                "Debug Erase Sequence is not implemented for non-ARM targets.",
            ));
        };

        let DebugSequence::Arm(ref debug_sequence) = self.target.debug_sequence else {
            unreachable!("This should never happen. Please file a bug if it does.");
        };

        let erase_sequence = debug_sequence
            .debug_erase_sequence()
            .ok_or(Error::Arm(ArmError::NotImplemented("Debug Erase Sequence")))?;

        tracing::info!("Trying Debug Erase Sequence");
        let erase_result = erase_sequence.erase_all(interface.deref_mut());

        match erase_result {
            Ok(()) => (),
            // In case this happens after unlock. Try to re-attach the probe once.
            Err(ArmError::ReAttachRequired) => {
                Self::reattach_arm_interface(interface, debug_sequence)?;
                // For re-setup debugging on all cores
                for core_state in &self.cores {
                    core_state.enable_arm_debug(interface.deref_mut())?;
                }
            }
            Err(e) => return Err(Error::Arm(e)),
        }
        tracing::info!("Device Erased Successfully");
        Ok(())
    }

    /// Reads all the available ARM CoresightComponents of the currently attached target.
    ///
    /// This will recursively parse the Romtable of the attached target
    /// and create a list of all the contained components.
    pub fn get_arm_components(
        &mut self,
        dp: DpAddress,
    ) -> Result<Vec<CoresightComponent>, ArmError> {
        let interface = self.get_arm_interface()?;

        get_arm_components(interface, dp)
    }

    /// Get the target description of the connected target.
    pub fn target(&self) -> &Target {
        &self.target
    }

    /// Configure the target and probe for serial wire view (SWV) tracing.
    pub fn setup_tracing(
        &mut self,
        core_index: usize,
        destination: TraceSink,
    ) -> Result<(), Error> {
        // Enable tracing on the target
        {
            let mut core = self.core(core_index)?;
            crate::architecture::arm::component::enable_tracing(&mut core)?;
        }

        let sequence_handle = match &self.target.debug_sequence {
            DebugSequence::Arm(sequence) => sequence.clone(),
            _ => unreachable!("Mismatch between architecture and sequence type!"),
        };

        let components = self.get_arm_components(DpAddress::Default)?;
        let interface = self.get_arm_interface()?;

        // Configure SWO on the probe when the trace sink is configured for a serial output. Note
        // that on some architectures, the TPIU is configured to drive SWO.
        match destination {
            TraceSink::Swo(ref config) => {
                interface.enable_swo(config)?;
            }
            TraceSink::Tpiu(ref config) => {
                interface.enable_swo(config)?;
            }
            TraceSink::TraceMemory => {}
        }

        sequence_handle.trace_start(interface, &components, &destination)?;
        crate::architecture::arm::component::setup_tracing(interface, &components, &destination)?;

        self.configured_trace_sink.replace(destination);

        Ok(())
    }

    /// Configure the target to stop emitting SWV trace data.
    #[tracing::instrument(skip(self))]
    pub fn disable_swv(&mut self, core_index: usize) -> Result<(), Error> {
        crate::architecture::arm::component::disable_swv(&mut self.core(core_index)?)
    }

    /// Begin tracing a memory address over SWV.
    pub fn add_swv_data_trace(&mut self, unit: usize, address: u32) -> Result<(), ArmError> {
        let components = self.get_arm_components(DpAddress::Default)?;
        let interface = self.get_arm_interface()?;
        crate::architecture::arm::component::add_swv_data_trace(
            interface,
            &components,
            unit,
            address,
        )
    }

    /// Stop tracing from a given SWV unit
    pub fn remove_swv_data_trace(&mut self, unit: usize) -> Result<(), ArmError> {
        let components = self.get_arm_components(DpAddress::Default)?;
        let interface = self.get_arm_interface()?;
        crate::architecture::arm::component::remove_swv_data_trace(interface, &components, unit)
    }

    /// Return the `Architecture` of the currently connected chip.
    pub fn architecture(&self) -> Architecture {
        match &self.interfaces {
            ArchitectureInterface::Arm(_) => Architecture::Arm,
            ArchitectureInterface::Jtag(_, ifaces) => {
                if let JtagInterface::Riscv(_) = &ifaces[0] {
                    Architecture::Riscv
                } else {
                    Architecture::Xtensa
                }
            }
        }
    }

    /// Clears all hardware breakpoints on all cores
    pub fn clear_all_hw_breakpoints(&mut self) -> Result<(), Error> {
        self.halted_access(|session| {
            { 0..session.cores.len() }.try_for_each(|n| {
                session
                    .core(n)
                    .and_then(|mut core| core.clear_all_hw_breakpoints())
            })
        })
    }
}

// This test ensures that [Session] is fully [Send] + [Sync].
const _: fn() = || {
    fn assert_impl_all<T: ?Sized + Send>() {}

    assert_impl_all::<Session>();
};

impl Drop for Session {
    #[tracing::instrument(name = "session_drop", skip(self))]
    fn drop(&mut self) {
        if let Err(err) = self.clear_all_hw_breakpoints() {
            tracing::warn!("Could not clear all hardware breakpoints: {:?}", err);
        }

        // Call any necessary deconfiguration/shutdown hooks.
        if let Err(err) = { 0..self.cores.len() }
            .try_for_each(|i| self.core(i).and_then(|mut core| core.debug_core_stop()))
        {
            tracing::warn!("Failed to deconfigure device during shutdown: {:?}", err);
        }
    }
}

/// Determine the [Target] from a [TargetSelector].
///
/// If the selector is [TargetSelector::Unspecified], the target will be looked up in the registry.
/// If it its [TargetSelector::Auto], probe-rs will try to determine the target automatically, based on
/// information read from the chip.
fn get_target_from_selector(
    target: TargetSelector,
    attach_method: AttachMethod,
    mut probe: Probe,
) -> Result<(Probe, Target), Error> {
    let target = match target {
        TargetSelector::Unspecified(name) => crate::config::get_target_by_name(name)?,
        TargetSelector::Specified(target) => target,
        TargetSelector::Auto => {
            // At this point we do not know what the target is, so we cannot use the chip specific reset sequence.
            // Thus, we try just using a normal reset for target detection if we want to do so under reset.
            // This can of course fail, but target detection is a best effort, not a guarantee!
            if AttachMethod::UnderReset == attach_method {
                probe.target_reset_assert()?;
            }
            probe.attach_to_unspecified()?;

            let (returned_probe, found_target) = crate::vendor::auto_determine_target(probe)?;
            probe = returned_probe;

            if AttachMethod::UnderReset == attach_method {
                // Now we can deassert reset in case we asserted it before.
                probe.target_reset_deassert()?;
            }

            if let Some(target) = found_target {
                target
            } else {
                return Err(Error::ChipNotFound(RegistryError::ChipAutodetectFailed));
            }
        }
    };

    Ok((probe, target))
}

/// The `Permissions` struct represents what a [Session] is allowed to do with a target.
/// Some operations can be irreversible, so need to be explicitly allowed by the user.
///
/// # Example
///
/// ```
/// use probe_rs::Permissions;
///
/// let permissions = Permissions::new().allow_erase_all();
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct Permissions {
    /// When set to true, all memory of the chip may be erased or reset to factory default
    erase_all: bool,
}

impl Permissions {
    /// Constructs a new permissions object with the default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow the session to erase all memory of the chip or reset it to factory default.
    ///
    /// # Warning
    /// This may irreversibly remove otherwise read-protected data from the device like security keys and 3rd party firmware.
    /// What happens exactly may differ per device and per probe-rs version.
    #[must_use]
    pub fn allow_erase_all(self) -> Self {
        Self {
            erase_all: true,
            ..self
        }
    }

    pub(crate) fn erase_all(&self) -> Result<(), MissingPermissions> {
        if self.erase_all {
            Ok(())
        } else {
            Err(MissingPermissions("erase_all".into()))
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("An operation could not be performed because it lacked the permission to do so: {0}")]
pub struct MissingPermissions(pub String);
