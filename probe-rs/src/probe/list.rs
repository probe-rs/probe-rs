use crate::{
    DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, Probe, ProbeCreationError,
};

use super::{
    cmsisdap,
    espusbjtag::{self, list_espjtag_devices},
    jlink::{self, list_jlink_devices},
    stlink, wlink,
};

#[cfg(feature = "ftdi")]
use super::ftdi;

/// Struct to list all attached debug probes
#[derive(Debug)]
pub struct Lister {
    lister: Box<dyn ProbeLister>,
}

impl Lister {
    /// Create a new lister with the default lister implementation.
    pub fn new() -> Self {
        Self {
            lister: Box::new(AllProbesLister::new()),
        }
    }

    /// Create a new lister with a custom lister implementation.
    pub fn with_lister(lister: Box<dyn ProbeLister>) -> Self {
        Self { lister }
    }

    /// Try to open a probe using the given selector
    pub fn open(&self, selector: impl Into<DebugProbeSelector>) -> Result<Probe, DebugProbeError> {
        self.lister.open(&selector.into())
    }

    /// List all available debug probes
    pub fn list_all(&self) -> Vec<DebugProbeInfo> {
        self.lister.list_all()
    }
}

impl Default for Lister {
    fn default() -> Self {
        Self::new()
    }
}

/// Trait for a probe lister implementation.
///
/// This trait can be used to implement custom probe listers.
pub trait ProbeLister: std::fmt::Debug {
    /// Try to open a probe using the given selector
    fn open(&self, selector: &DebugProbeSelector) -> Result<Probe, DebugProbeError>;

    /// List all probes found by the lister.
    fn list_all(&self) -> Vec<DebugProbeInfo>;
}

#[derive(Debug, PartialEq, Eq)]
pub struct AllProbesLister;

impl ProbeLister for AllProbesLister {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Probe, DebugProbeError> {
        Self::open(selector)
    }

    fn list_all(&self) -> Vec<DebugProbeInfo> {
        Self::list_all()
    }
}

impl Default for AllProbesLister {
    fn default() -> Self {
        Self::new()
    }
}

impl AllProbesLister {
    pub fn new() -> Self {
        Self
    }

    fn open(selector: impl Into<DebugProbeSelector>) -> Result<Probe, DebugProbeError> {
        let selector = selector.into();
        match cmsisdap::CmsisDap::new_from_selector(selector.clone()) {
            Ok(link) => return Ok(Probe::from_specific_probe(link)),
            Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
            Err(e) => return Err(e),
        };
        #[cfg(feature = "ftdi")]
        match ftdi::FtdiProbe::new_from_selector(selector.clone()) {
            Ok(link) => return Ok(Probe::from_specific_probe(link)),
            Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
            Err(e) => return Err(e),
        };
        match stlink::StLink::new_from_selector(selector.clone()) {
            Ok(link) => return Ok(Probe::from_specific_probe(link)),
            Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
            Err(e) => return Err(e),
        };
        match jlink::JLink::new_from_selector(selector.clone()) {
            Ok(link) => return Ok(Probe::from_specific_probe(link)),
            Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
            Err(e) => return Err(e),
        };
        match espusbjtag::EspUsbJtag::new_from_selector(selector.clone()) {
            Ok(link) => return Ok(Probe::from_specific_probe(link)),
            Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
            Err(e) => return Err(e),
        };
        match wlink::WchLink::new_from_selector(selector) {
            Ok(link) => return Ok(Probe::from_specific_probe(link)),
            Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
            Err(e) => return Err(e),
        };

        Err(DebugProbeError::ProbeCouldNotBeCreated(
            ProbeCreationError::NotFound,
        ))
    }

    fn list_all() -> Vec<DebugProbeInfo> {
        let mut list = cmsisdap::tools::list_cmsisdap_devices();
        #[cfg(feature = "ftdi")]
        {
            list.extend(ftdi::list_ftdi_devices());
        }
        list.extend(stlink::tools::list_stlink_devices());

        list.extend(list_jlink_devices());

        list.extend(list_espjtag_devices());

        list.extend(wlink::list_wlink_devices());

        list
    }
}
