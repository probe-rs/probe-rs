use crate::probe::debug_probe::MasterProbe;
use crate::probe::target::Target;

pub struct Session {
    pub target: Target,
    pub probe: MasterProbe,
}

impl Session {
    /// Open a new session with a given debug target
    pub fn new(target: Target, probe: MasterProbe) -> Self {
        Self {
            target: target,
            probe: probe,
        }
    }
}