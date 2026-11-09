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

/// Returns the index of the probe to use when none is selected: the first one, if all listed
/// probes are one device. That is one probe that several drivers list, or the interfaces of one
/// probe.
pub fn default_probe_index<'a>(
    probes: impl IntoIterator<Item = &'a DebugProbeInfo>,
) -> Option<usize> {
    let mut probes = probes.into_iter();
    let first = probes.next()?;
    probes.all(|probe| same_device(first, probe)).then_some(0)
}

/// Whether two listed probes are the same USB device, as far as their location or serial number
/// tells.
fn same_device(a: &DebugProbeInfo, b: &DebugProbeInfo) -> bool {
    (a.vendor_id, a.product_id) == (b.vendor_id, b.product_id)
        && match (&a.usb_location, &b.usb_location) {
            (Some(a), Some(b)) => a == b,
            _ => a.serial_number.is_some() && a.serial_number == b.serial_number,
        }
}

/// Moves the entries of a device that several drivers list next to its first entry. Devices keep
/// the order of their first entry.
fn group_by_device(list: Vec<ProbeListItem>) -> Vec<ProbeListItem> {
    let mut groups: Vec<Vec<ProbeListItem>> = Vec::new();
    for item in list {
        match groups
            .iter_mut()
            .find(|group| same_device(&group[0].info, &item.info))
        {
            Some(group) => group.push(item),
            None => groups.push(vec![item]),
        }
    }
    groups.into_iter().flatten().collect()
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

        group_by_device(list)
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

#[cfg(test)]
mod tests {
    use super::{ProbeListItem, default_probe_index, group_by_device};
    use crate::probe::{DebugProbeInfo, fake_probe::FakeProbeFactory};

    fn probe(serial: Option<&str>, interface: Option<u8>) -> DebugProbeInfo {
        let serial = serial.map(str::to_string);
        DebugProbeInfo::new(
            "probe",
            0x0451,
            0xbef3,
            serial,
            &FakeProbeFactory,
            interface,
            false,
        )
    }

    #[test]
    fn one_device_listed_twice_is_the_default() {
        let probes = [probe(Some("A"), Some(2)), probe(Some("A"), Some(5))];
        assert_eq!(default_probe_index(&probes), Some(0));
    }

    #[test]
    fn two_devices_have_no_default() {
        let probes = [probe(Some("A"), Some(2)), probe(Some("B"), Some(2))];
        assert_eq!(default_probe_index(&probes), None);

        // Without a serial number or a USB location, two probes can't be told apart.
        let probes = [probe(None, None), probe(None, None)];
        assert_eq!(default_probe_index(&probes), None);

        let mut probes = [probe(Some("A"), None), probe(Some("A"), None)];
        probes[0].usb_location = Some(("1".to_string(), 1));
        probes[1].usb_location = Some(("1".to_string(), 2));
        assert_eq!(default_probe_index(&probes), None);
    }

    #[test]
    fn entries_of_one_device_are_grouped() {
        let list = [
            probe(Some("A"), Some(2)),
            probe(Some("B"), Some(2)),
            probe(None, None),
            probe(Some("A"), Some(5)),
            probe(Some("B"), Some(5)),
            probe(None, None),
        ]
        .map(ProbeListItem::accessible);

        let grouped: Vec<_> = group_by_device(list.to_vec())
            .into_iter()
            .map(|item| (item.info.serial_number, item.info.interface))
            .collect();
        let a = || Some("A".to_string());
        let b = || Some("B".to_string());
        assert_eq!(
            grouped,
            [
                (a(), Some(2)),
                (a(), Some(5)),
                (b(), Some(2)),
                (b(), Some(5)),
                (None, None),
                (None, None),
            ]
        );
    }
}
