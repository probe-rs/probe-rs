use crate::config::target::Target;
use crate::probe::{DebugProbeError, MasterProbe};

pub struct Session {
    pub target: Target,
    pub probe: MasterProbe,

    hw_breakpoint_enabled: bool,
    active_breakpoints: Vec<Breakpoint>,
}

impl Session {
    /// Open a new session with a given debug target
    pub fn new(target: Target, probe: MasterProbe) -> Self {
        Self {
            target,
            probe,
            hw_breakpoint_enabled: false,
            active_breakpoints: Vec::new(),
        }
    }

    pub fn builder() -> SessionBuilder {
        SessionBuilder {
            target: None,
            probe: None,
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

pub enum SessionBuilderError {
    NoTargetSpecified,
    NoProbeSpecified,
    NoProbeFound,
    ChipAutodetectFailed,
}

/// A builder for the [Session](struct.Session.html) struct.
/// It can be used to conveniently create a new session.
pub struct SessionBuilder {
    target: Option<Target>,
    probe: Option<MasterProbe>,
}

impl SessionBuilder {
    pub fn with_discovered_probe(mut self) -> Result<SessionBuilder, SessionBuilderError> {
        let probes = MasterProbe::list_all();
        if probes.len() == 1 {
            match MasterProbe::from_probe_info(&probes[0]) {
                Ok(probe) => self.probe = Some(probe),
                Err(e) => {
                    log::error!("{}", e);
                    return Err(SessionBuilderError::NoProbeFound);
                }
            }
        }
        Ok(self)
    }

    pub fn with_specific_probe(
        mut self,
        probe: MasterProbe,
    ) -> Result<SessionBuilder, SessionBuilderError> {
        self.probe = Some(probe);
        Ok(self)
    }

    // pub fn with_discovered_target(self) -> Result<SessionBuilder, SessionBuilderError> {
    //     let registry = Registry::from_builtin_families();

    //     let target = registry
    //         .get_target(SelectionStrategy::ChipInfo(ChipInfo::read_from_rom_table(&mut probe))
    //         .map_err(|_| "Failed to find target")?;
    // }

    pub fn with_specific_target(
        mut self,
        target: Target,
    ) -> Result<SessionBuilder, SessionBuilderError> {
        self.target = Some(target);
        Ok(self)
    }

    /// Tries to build the Session from the stored parameters.
    pub fn build(self) -> Result<Session, SessionBuilderError> {
        Ok(Session::new(
            self.target.ok_or(SessionBuilderError::NoTargetSpecified)?,
            self.probe.ok_or(SessionBuilderError::NoProbeSpecified)?,
        ))
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
