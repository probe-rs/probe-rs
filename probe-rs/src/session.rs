use crate::config::registry::{Registry, SelectionStrategy};
use crate::config::target::Target;
use crate::probe::{DebugProbeError, MasterProbe};
use crate::target::info::ChipInfo;

/// The session object holds all the necessary information to make your debugging session as enjoyable as possible.
///
/// It holds a [MasterProbe](/probe_rs/probe/struct.MasterProbe) and a debug [Target](Target).
///
/// It also keeps track of all active and remaining free breakpoints.
///
/// Use a [SessionBuilder](SessionBuilder) to acquire this struct.
/// Keep in mind that the builder evaluates lazily on `[build()](SessionBuilder::build)`.
///
/// ```rust
/// // Create a new session with autodiscovery for probe and target.
/// // Observe how the ordering of with_discovered_target() and with_discovered_probe()
/// // does not matter, as it will always be attempted to create the probe before the target is created.
/// # use probe_rs::session::SessionBuilderError;
/// use probe_rs::session::Session;
/// # fn _main() -> Result<(), SessionBuilderError> {
/// let session = Session::builder().with_discovered_target().with_discovered_probe().build()?;
/// # Ok(())
/// # }
/// ```
pub struct Session {
    pub target: Target,
    pub probe: MasterProbe,

    hw_breakpoint_enabled: bool,
    active_breakpoints: Vec<Breakpoint>,
}

impl Session {
    /// Creates a new [Session](Session) with [MasterProbe](/probe_rs/probe/struct.MasterProbe) and a given debug [Target](Target).
    pub fn new(target: Target, probe: MasterProbe) -> Self {
        Self {
            target,
            probe,
            hw_breakpoint_enabled: false,
            active_breakpoints: Vec::new(),
        }
    }

    /// Creates a new [SessionBuilder](SessionBuilder).
    pub fn builder() -> SessionBuilder {
        SessionBuilder {
            target_creator: Box::new(|_| Err(SessionBuilderError::NoTargetCreator)),
            probe_creator: Box::new(|| Err(SessionBuilderError::NoProbeCreator)),
        }
    }

    /// Set a hardware breakpoint
    pub fn set_hw_breakpoint(&mut self, address: u32) -> Result<(), DebugProbeError> {
        log::debug!("Trying to set HW breakpoint at address {:#08x}", address);

        // Get the number of HW breakpoints available
        let num_hw_breakpoints =
            self.target
                .core
                .get_available_breakpoint_units(&mut self.probe)? as usize;

        log::debug!("{} HW breakpoints are supported.", num_hw_breakpoints);

        if num_hw_breakpoints <= self.active_breakpoints.len() {
            // We cannot set additional breakpoints
            log::warn!("Maximum number of breakpoints ({}) reached, unable to set additional HW breakpoint.", num_hw_breakpoints);

            // TODO: Better error here
            return Err(DebugProbeError::UnknownError);
        }

        if !self.hw_breakpoint_enabled {
            self.target.core.enable_breakpoints(&mut self.probe, true)?;
            self.hw_breakpoint_enabled = true;
        }

        let bp_unit = self.find_free_breakpoint_unit();

        log::debug!("Using comparator {} of breakpoint unit", bp_unit);
        // actually set the breakpoint
        self.target
            .core
            .set_breakpoint(&mut self.probe, bp_unit, address)?;

        self.active_breakpoints.push(Breakpoint {
            address,
            register_hw: bp_unit,
        });

        Ok(())
    }

    pub fn clear_hw_breakpoint(&mut self, address: u32) -> Result<(), DebugProbeError> {
        let bp_position = self
            .active_breakpoints
            .iter()
            .position(|bp| bp.address == address);

        match bp_position {
            Some(bp_position) => {
                let bp = &self.active_breakpoints[bp_position];
                self.target
                    .core
                    .clear_breakpoint(&mut self.probe, bp.register_hw)?;

                // We only remove the breakpoint if we have actually managed to clear it.
                self.active_breakpoints.swap_remove(bp_position);
                Ok(())
            }
            None => Err(DebugProbeError::UnknownError),
        }
    }

    fn find_free_breakpoint_unit(&self) -> usize {
        let mut used_bp: Vec<_> = self
            .active_breakpoints
            .iter()
            .map(|bp| bp.register_hw)
            .collect();
        used_bp.sort();

        let mut free_bp = 0;

        for bp in used_bp {
            if bp == free_bp {
                free_bp += 1;
            } else {
                return free_bp;
            }
        }

        free_bp
    }
}

#[derive(Debug, Copy, Clone)]
pub enum SessionBuilderError {
    NoProbeFound,
    UnknownError,
    MultipleProbesFound,
    NoTargetCreator,
    NoProbeCreator,
}

/// A builder for the [Session](struct.Session.html) struct.
/// It should be used to conveniently create a new session.
///
/// The builder is evaluated lazily. Only when [build()](SessionBuilder::build) is called,
/// the stored creators are called.
pub struct SessionBuilder {
    target_creator: Box<dyn FnOnce(&mut MasterProbe) -> Result<Target, SessionBuilderError>>,
    probe_creator: Box<dyn FnOnce() -> Result<MasterProbe, SessionBuilderError>>,
}

impl SessionBuilder {
    /// Prepares the `SessionBuilder` to use the currently connected probe.
    ///
    /// If exactly one probe is discovered it will be used in the session.
    ///
    /// If multiple probes are discovered a `SessionBuilderError::MultipleProbesFound`
    /// will be will be generated during [build()](SessionBuilder::build).
    ///
    /// If no probe is found, a `SessionBuilderError::NoProbeFound`
    /// will be generated during [build()](SessionBuilder::build).
    ///
    /// If anything goes wrong during probe creation, a `SessionBuildError::UnknownError`
    /// will be generated during [build()](SessionBuilder::build).
    /// Check the log for more information.
    pub fn with_discovered_probe(mut self) -> SessionBuilder {
        self.probe_creator = Box::new(|| {
            let probes = MasterProbe::list_all();
            match probes.len() {
                1 => match MasterProbe::from_probe_info(&probes[0]) {
                    Ok(probe) => Ok(probe),
                    Err(e) => {
                        log::error!("{}", e);
                        Err(SessionBuilderError::UnknownError)
                    }
                },
                0 => Err(SessionBuilderError::NoProbeFound),
                _ => Err(SessionBuilderError::MultipleProbesFound),
            }
        });
        self
    }

    /// Prepares the `SessionBuilder` to select the probe returned by the creator.
    pub fn with_probe_creator(
        mut self,
        creator: impl FnOnce() -> Result<MasterProbe, SessionBuilderError> + 'static,
    ) -> SessionBuilder {
        self.probe_creator = Box::new(creator);
        self
    }

    /// Prepares the `SessionBuilder` to use the discovered target.
    ///
    /// If target discovery or creation goes wrong, a `SessionBuildError::UnknownError`
    /// will be generated during [build()](SessionBuilder::build).
    /// Check the log for more information.
    ///
    /// If no probe could be created on the `SessionBuilder` resolve,
    /// this creator will never be called.
    pub fn with_discovered_target(mut self) -> SessionBuilder {
        self.target_creator = Box::new(|probe| {
            let registry = Registry::from_builtin_families();

            let chip_info = match ChipInfo::read_from_rom_table(probe) {
                Ok(chip_info) => chip_info,
                Err(e) => {
                    log::error!("{}", e);
                    return Err(SessionBuilderError::UnknownError);
                }
            };
            match registry.get_target(SelectionStrategy::ChipInfo(chip_info)) {
                Ok(target) => Ok(target),
                Err(e) => {
                    log::error!("{}", e);
                    Err(SessionBuilderError::UnknownError)
                }
            }
        });
        self
    }

    /// Prepares the `SessionBuilder` to select the probe returned by the creator.
    ///
    /// Once the `SessionBuilder` resolves, it will first try and create the probe.
    /// Then it will pass the created probe to the target creator, if it succeeded.
    pub fn with_target_creator(
        mut self,
        creator: impl FnOnce(&mut MasterProbe) -> Result<Target, SessionBuilderError> + 'static,
    ) -> SessionBuilder {
        self.target_creator = Box::new(creator);
        self
    }

    /// Tries to build the `Session` from the stored parameters.
    /// First the `SessionBuilder` tries to create the `MasterProbe`.
    /// If anything goes wrong, the error will immediately be returned.
    /// If the probe is successfully created, and only if, the `SessionBuilder`
    /// tries to create the `Target`.
    pub fn build(self) -> Result<Session, SessionBuilderError> {
        let mut probe = (self.probe_creator)()?;
        let target = (self.target_creator)(&mut probe)?;
        Ok(Session::new(target, probe))
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct BreakpointId(usize);

impl BreakpointId {
    pub fn new(id: usize) -> Self {
        BreakpointId(id)
    }
}

struct Breakpoint {
    address: u32,
    register_hw: usize,
}
