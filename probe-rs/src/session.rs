use crate::probe::debug_probe::MasterProbe;
use crate::probe::flash::flasher::FlashAlgorithm;
use crate::target::Target;

pub struct Session {
    pub target: Target,
    pub probe: MasterProbe,
    pub flash_algorithm: Option<FlashAlgorithm>,
}

impl Session {
    /// Open a new session with a given debug target
    pub fn new(
        target: Target,
        probe: MasterProbe,
        flash_algorithm: Option<FlashAlgorithm>,
    ) -> Self {
        Self {
            target,
            probe,
            flash_algorithm,
        }
    }
}
