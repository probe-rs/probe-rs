//! Listing probes of various types.

use crate::probe::{
    DebugProbeError, DebugProbeInfo, DebugProbeSelector, Probe, ProbeCreationError,
};

use super::DRIVERS;

/// Whether the current process can access a listed probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Accessibility {
    /// The probe can be accessed.
    #[default]
    Accessible,
    /// The probe was found but the current user is not allowed to access it.
    PermissionDenied,
}

/// A probe found during listing, together with whether it can be accessed.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeListItem {
    /// The probe that was found.
    pub info: DebugProbeInfo,
    /// Whether the current process can access the probe's underlying device.
    pub accessibility: Accessibility,
}

impl ProbeListItem {
    /// Creates a list item for a probe that is accessible.
    pub fn accessible(info: DebugProbeInfo) -> Self {
        Self {
            info,
            accessibility: Accessibility::Accessible,
        }
    }
}

/// Returns whether the current process can access the USB probe backed by the given device.
///
/// On platforms other than Linux this always returns [`Accessibility::Accessible`].
pub fn usb_probe_accessibility(device: &nusb::DeviceInfo) -> Accessibility {
    #[cfg(target_os = "linux")]
    let accessibility = {
        let path = linux::usbfs_node_path(device.busnum(), device.device_address());
        device_node_accessibility(&path)
    };
    #[cfg(not(target_os = "linux"))]
    let accessibility = {
        let _ = device;
        Accessibility::Accessible
    };
    accessibility
}

/// Returns whether the current process can access the probe reached through the given device node.
///
/// Use this for probes opened through a filesystem path, such as a serial port.
/// On platforms other than Linux this always returns [`Accessibility::Accessible`].
pub fn device_node_accessibility(path: &str) -> Accessibility {
    #[cfg(target_os = "linux")]
    {
        if !linux::can_access(path) {
            return Accessibility::PermissionDenied;
        }
    }
    let _ = path;
    Accessibility::Accessible
}

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
        self.list(None)
    }

    /// List probes found by the lister, with optional filtering.
    pub fn list(&self, selector: Option<&DebugProbeSelector>) -> Vec<DebugProbeInfo> {
        self.lister.list(selector)
    }

    /// List all available debug probes, annotating each with whether it can be accessed.
    pub fn list_all_with_access(&self) -> Vec<ProbeListItem> {
        self.list_with_access(None)
    }

    /// List probes with optional filtering, annotating each with whether it can be accessed.
    ///
    /// Probes that cannot be accessed are still included, marked
    /// [`Accessibility::PermissionDenied`].
    pub fn list_with_access(&self, selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
        self.lister.list_with_access(selector)
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

    /// List probes found by the lister, annotating each with whether it can be accessed.
    fn list_with_access(&self, selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem>;

    /// List probes found by the lister, with optional filtering.
    fn list(&self, selector: Option<&DebugProbeSelector>) -> Vec<DebugProbeInfo> {
        self.list_with_access(selector)
            .into_iter()
            .map(|item| item.info)
            .collect()
    }

    /// List all probes found by the lister.
    fn list_all(&self) -> Vec<DebugProbeInfo> {
        self.list(None)
    }
}

/// Default lister implementation that includes all built-in probe drivers.
#[derive(Debug, PartialEq, Eq)]
pub struct AllProbesLister;

impl ProbeLister for AllProbesLister {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Probe, DebugProbeError> {
        let mut open_error = None;
        let mut fallback_error = ProbeCreationError::NotFound;

        for probe_ctor in DRIVERS.read().iter() {
            match probe_ctor.open(selector) {
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

    fn list_with_access(&self, selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
        let mut list = vec![];

        for driver in DRIVERS.read().iter() {
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
    /// Create a new lister with all built-in probe drivers.
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(target_os = "linux")]
mod linux {
    /// Builds the usbfs device node path for a bus/device number pair.
    pub(super) fn usbfs_node_path(busnum: u8, device_address: u8) -> String {
        format!("/dev/bus/usb/{busnum:03}/{device_address:03}")
    }

    /// Returns whether the current process can read and write the given device node.
    ///
    /// This reflects the permissions and ACLs actually applied to the node, so it is
    /// agnostic to how they got there (udev rule, group mode, `uaccess`, ...).
    pub(super) fn can_access(path: &str) -> bool {
        use nix::unistd::{AccessFlags, access};
        // access(2) with R_OK|W_OK checks the real uid/gid against the node's perms and ACLs.
        access(path, AccessFlags::R_OK | AccessFlags::W_OK).is_ok()
    }

    #[cfg(test)]
    mod tests {
        use super::usbfs_node_path;

        #[test]
        fn node_path_is_zero_padded() {
            assert_eq!(usbfs_node_path(1, 5), "/dev/bus/usb/001/005");
            assert_eq!(usbfs_node_path(12, 127), "/dev/bus/usb/012/127");
        }
    }
}
