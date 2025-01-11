//! Listing probes of various types.

use crate::probe::{
    DebugProbeError, DebugProbeInfo, DebugProbeSelector, Probe, ProbeCreationError, ProbeFactory,
};

use super::{blackmagic, cmsisdap, espusbjtag, jlink, stlink, wlink};

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
    pub async fn open(
        &self,
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Probe, DebugProbeError> {
        self.lister.open(selector.into()).await
    }

    /// List all available debug probes
    pub async fn list_all(&self) -> Vec<DebugProbeInfo> {
        self.lister.list_all().await
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
#[async_trait::async_trait(?Send)]
pub trait ProbeLister: std::fmt::Debug {
    /// Try to open a probe using the given selector
    async fn open(&self, selector: DebugProbeSelector) -> Result<Probe, DebugProbeError>;

    /// List all probes found by the lister.
    async fn list_all(&self) -> Vec<DebugProbeInfo>;
}

/// Default lister implementation that includes all built-in probe drivers.
#[derive(Debug, PartialEq, Eq)]
pub struct AllProbesLister;

#[async_trait::async_trait(?Send)]
impl ProbeLister for AllProbesLister {
    async fn open(&self, selector: DebugProbeSelector) -> Result<Probe, DebugProbeError> {
        Self::open(selector).await
    }

    async fn list_all(&self) -> Vec<DebugProbeInfo> {
        Self::list_all().await
    }
}

impl Default for AllProbesLister {
    fn default() -> Self {
        Self::new()
    }
}

impl AllProbesLister {
    const DRIVERS: &'static [&'static dyn ProbeFactory] = &[
        &blackmagic::BlackMagicProbeFactory,
        &cmsisdap::CmsisDapFactory,
        // TODO:
        // &ftdi::FtdiProbeFactory,
        &stlink::StLinkFactory,
        &jlink::JLinkFactory,
        &espusbjtag::EspUsbJtagFactory,
        &wlink::WchLinkFactory,
    ];

    /// Create a new lister with all built-in probe drivers.
    pub const fn new() -> Self {
        Self
    }

    async fn open(selector: impl Into<DebugProbeSelector>) -> Result<Probe, DebugProbeError> {
        let selector = selector.into();

        for probe_ctor in Self::DRIVERS {
            match probe_ctor.open(selector.clone()).await {
                Ok(link) => return Ok(Probe::from_specific_probe(link)),
                Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
                Err(e) => return Err(e),
            };
        }

        Err(DebugProbeError::ProbeCouldNotBeCreated(
            ProbeCreationError::NotFound,
        ))
    }

    async fn list_all() -> Vec<DebugProbeInfo> {
        let mut list = vec![];

        for driver in Self::DRIVERS {
            list.extend(driver.list_probes().await);
        }

        list
    }
}

/// Lists all USB devices that are plugged in and found by the system.
pub async fn list_devices() -> Result<impl Iterator<Item = nusb::DeviceInfo>, nusb::Error> {
    nusb::list_devices().await
}
