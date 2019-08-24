use probe::debug_probe::MasterProbe;
use probe::target::Target;

pub struct Session {
    pub target: Box<Target>,
    pub probe: MasterProbe,
}

impl Session {
    /// Open a new session with a given debug target
    pub fn new(target: impl Target + 'static, probe: MasterProbe) -> Self {
        Self {
            target: Box::new(target),
            probe: probe,
        }
    }
}