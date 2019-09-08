use probe::debug_probe::MasterProbe;
use probe::target::Target;

pub struct Session {
    pub target: Box<dyn Target>,
    pub probe: MasterProbe,
}

impl Session {
    /// Open a new session with a given debug target
    pub fn new(target: Box<dyn Target>, probe: MasterProbe) -> Self {
        Self {
            target: target,
            probe: probe,
        }
    }
}