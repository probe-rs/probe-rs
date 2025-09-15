//! Listing probes of various types.

use crate::probe::{
    DebugProbeError, DebugProbeInfo, DebugProbeSelector, Probe, ProbeCreationError, ProbeFactory,
};

use super::{
    blackmagic, ch347usbjtag, cmsisdap, espusbjtag, ftdi, glasgow, jlink, sifliuart, stlink, wlink,
};

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

    /// List all available debug probes
    pub fn list(&self, selector: Option<&DebugProbeSelector>) -> Vec<DebugProbeInfo> {
        self.lister.list(selector)
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
    fn list_all(&self) -> Vec<DebugProbeInfo> {
        self.list(None)
    }

    /// List probes found by the lister, with optional filtering.
    fn list(&self, selector: Option<&DebugProbeSelector>) -> Vec<DebugProbeInfo>;
}

/// Default lister implementation that includes all built-in probe drivers.
#[derive(Debug, PartialEq, Eq)]
pub struct AllProbesLister;

impl ProbeLister for AllProbesLister {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Probe, DebugProbeError> {
        let selector = selector.into();

        let mut open_error = None;
        let mut fallback_error = ProbeCreationError::NotFound;

        for probe_ctor in Self::DRIVERS {
            match probe_ctor.open(&selector) {
                Ok(link) => return Ok(Probe::from_specific_probe(link)),
                Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
                Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::CouldNotOpen)) => {
                    fallback_error = ProbeCreationError::CouldNotOpen;
                }
                Err(e) => open_error = Some(e),
            };
        }

        Err(open_error.unwrap_or(DebugProbeError::ProbeCouldNotBeCreated(fallback_error)))
    }

    fn list(&self, selector: Option<&DebugProbeSelector>) -> Vec<DebugProbeInfo> {
        let mut list = vec![];

        for driver in Self::DRIVERS {
            list.extend(driver.list_probes_filtered(selector));
        }

        list
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
        &ftdi::FtdiProbeFactory,
        &stlink::StLinkFactory,
        &jlink::JLinkFactory,
        &espusbjtag::EspUsbJtagFactory,
        &wlink::WchLinkFactory,
        &sifliuart::SifliUartFactory,
        &glasgow::GlasgowFactory,
        &ch347usbjtag::Ch347UsbJtagFactory,
    ];

    /// Create a new lister with all built-in probe drivers.
    pub const fn new() -> Self {
        Self
    }
}
