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

    /// List probes found by the lister, with optional filtering.
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

                    #[cfg(target_os = "linux")]
                    linux::help_linux();
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

        #[cfg(target_os = "linux")]
        if list.is_empty() {
            linux::help_linux();
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

#[cfg(target_os = "linux")]
mod linux {
    use std::process::Command;

    const SYSTEMD_SUPPORT_UACCESS_VERSION: usize = 30;
    const UDEV_RULES_PATH: &str = "/etc/udev/rules.d";

    /// Gives the user a hint if they are on Linux.
    ///
    /// Best is to call this only if no probes were found.
    pub(super) fn help_linux() {
        if std::env::var("PROBE_RS_DISABLE_SETUP_HINTS").is_ok() {
            return;
        }

        help_systemd();
        help_udev_rules();
    }

    /// Prints a helptext if udev rules seem to be missing.
    fn help_udev_rules() {
        if !udev_rule_present() {
            tracing::warn!("There seems no probe-rs rule to be installed.");
            tracing::warn!("Read more under https://probe.rs/docs/getting-started/probe-setup/");
            tracing::warn!(
                "If you manage your rules differently, put an empty rule file with 'probe-rs' in the name in {UDEV_RULES_PATH}."
            );
        }
    }

    /// Prints a helptext if udev user groups seem to be missing or wrong.
    fn help_systemd() {
        let systemd_version = systemd_version();

        if systemd_version.unwrap_or_default() < SYSTEMD_SUPPORT_UACCESS_VERSION {
            tracing::warn!(
                "The systemd on your Linux is older than v30, which doesn't support uaccess mechanism"
            );
        }
    }

    /// Returns the systemd version of the current system.
    fn systemd_version() -> Option<usize> {
        let output = match Command::new("systemctl").arg("--version").output() {
            Err(error) => {
                tracing::debug!("Gathering information about relevant user groups failed: {error}");
                return None;
            }
            Ok(child) => child,
        };
        if !output.status.success() {
            tracing::debug!(
                "Gathering information about relevant user groups failed: {:?}",
                output.status.code()
            );
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        // First line looks like: "systemd 256 (256.6-1-arch)"
        stdout
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|version| version.parse().ok())
    }

    /// Returns true if there is a probe-rs resembling udev rule file.
    fn udev_rule_present() -> bool {
        let mut files = match std::fs::read_dir(UDEV_RULES_PATH) {
            Err(error) => {
                tracing::debug!("Listing udev rule files at {UDEV_RULES_PATH} failed: {error}");
                return false;
            }
            Ok(files) => files,
        };

        files.any(|p| p.unwrap().path().display().to_string().contains("probe-rs"))
    }
}
