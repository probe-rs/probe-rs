use crate::config::target::Target;
use crate::probe::{DebugProbeError, MasterProbe};

pub struct Session {
    pub target: Target,
    pub probe: MasterProbe,

    hw_breakpoint_enabled: bool,
    active_breakpoints: Vec<Breakpoint>,
    bp_id_count: usize,
}

impl Session {
    /// Open a new session with a given debug target
    pub fn new(target: Target, probe: MasterProbe) -> Self {
        Self {
            target,
            probe,
            hw_breakpoint_enabled: false,
            active_breakpoints: Vec::new(),
            bp_id_count: 0,
        }
    }

    /// Set a hardware breakpoint
    pub fn set_hw_breakpoint(&mut self, address: u32) -> Result<BreakpointId, DebugProbeError> {
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

        let id = BreakpointId(self.bp_id_count);
        self.bp_id_count += 1;

        self.active_breakpoints.push(Breakpoint {
            id,
            register_hw: bp_unit,
        });

        Ok(id)
    }

    pub fn clear_hw_breakpoint(&mut self, id: BreakpointId) -> Result<(), DebugProbeError> {
        let bp_position = self.active_breakpoints.iter().position(|bp| bp.id == id);

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

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct BreakpointId(usize);

impl BreakpointId {
    pub fn new(id: usize) -> Self {
        BreakpointId(id)
    }
}

struct Breakpoint {
    id: BreakpointId,
    register_hw: usize,
}
