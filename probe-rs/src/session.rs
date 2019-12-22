use crate::config::target::Target;
use crate::probe::MasterProbe;

/// The `session` struct is the main interface to probe-rs.
///
/// # Creation
///
/// To create a new session, you need to specify a `Target` and a `MasterProbe`.
///
/// ```
/// use probe_rs::probe::{MasterProbe, daplink, DebugProbe};
/// use probe_rs::probe::daplink::DAPLink;
/// use probe_rs::config::registry::{Registry, SelectionStrategy, TargetIdentifier};
/// use probe_rs::{Session, Error};
///
/// # fn main() -> Result<(), Error> {
///
/// let registry = Registry::from_builtin_families();
/// let target = registry.get_target(SelectionStrategy::TargetIdentifier(TargetIdentifier {
///    chip_name: "nrf52".to_owned(),
///    flash_algorithm_name: None,
/// }))?;
///
/// let probes = daplink::tools::list_daplink_devices();
///
/// let specific_probe = DAPLink::new_from_probe_info(probes[0])?;
///
/// let probe = MasterProbe::from_specific_probe(specific_probe);
///
/// let session = Session::new(target, probe);
/// # Ok(())
/// # }
/// ```
pub struct Session {
    pub target: Target,
    pub probe: MasterProbe,
}

impl Session {
    /// Open a new session with a given debug target
    pub fn new(target: Target, probe: MasterProbe) -> Self {
        Self { target, probe }
    }
}

/*
pub struct SessionBuilder {

}

impl SessionBuilder {
    pub fn new() -> Self {
        Self {}
    }

    pub fn with_probe(&mut self) -> &mut Self {

    }

    pub fn with_target(&mut self) -> &mut Self {

    }

    pub fn with_default_probe(&mut self) -> &mut Self {

    }
}
*/
