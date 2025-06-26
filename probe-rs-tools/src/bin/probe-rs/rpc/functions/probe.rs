use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::{
    Session,
    probe::{
        DebugProbeInfo, DebugProbeKind as PRDebugProbeKind,
        DebugProbeSelector as PRDebugProbeSelector, UsbFilters as PRUsbFilters,
    },
};
use serde::{Deserialize, Serialize};

use crate::{
    rpc::{
        Key,
        functions::{RpcContext, RpcResult},
    },
    util::common_options::{OperationError, ProbeOptions},
};

#[cfg(any(target_os = "linux", target_os = "android"))]
use std::path::PathBuf;
use std::{fmt::Display, net::SocketAddr};

// Separate from DebugProbeInfo because we can't serialize a &dyn ProbeFactory
#[derive(Debug, Serialize, Deserialize, Clone, Schema)]
pub struct DebugProbeEntry {
    /// The name of the debug probe.
    pub identifier: String,
    /// The kind of probe.
    pub kind: DebugProbeKind,

    pub probe_type: String,
}

impl Display for DebugProbeEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} -- {} ({})",
            self.identifier, self.kind, self.probe_type,
        )
    }
}

impl From<DebugProbeInfo> for DebugProbeEntry {
    fn from(probe: DebugProbeInfo) -> DebugProbeEntry {
        DebugProbeEntry {
            probe_type: probe.probe_type(),
            identifier: probe.identifier,
            kind: probe.kind.into(),
        }
    }
}

impl DebugProbeEntry {
    pub fn selector(&self) -> DebugProbeSelector {
        self.kind.clone().into()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Schema)]
/// Represents what kind of probe is backing up a `DebugProbeInfo` instance.
pub enum DebugProbeKind {
    /// Physical USB device.
    Usb {
        /// The the USB vendor id of the debug probe to be used.
        vendor_id: u16,
        /// The the USB product id of the debug probe to be used.
        product_id: u16,
        /// USB filters.
        filters: UsbFilters,
    },
    /// Network socket device.
    SocketAddr(SocketAddr),
    #[cfg_attr(
        any(target_os = "linux", target_os = "android"),
        doc = "Unix socket device."
    )]
    #[cfg(any(target_os = "linux", target_os = "android"))]
    UnixSocketAddr(PathBuf),
}

impl std::fmt::Display for DebugProbeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DebugProbeKind::Usb {
                vendor_id,
                product_id,
                filters,
            } => write!(f, "{vendor_id:04x}:{product_id:04x}{filters}"),
            DebugProbeKind::SocketAddr(socket) => write!(f, "{socket}"),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            DebugProbeKind::UnixSocketAddr(path) => write!(f, "{}", path.display()),
        }
    }
}

impl From<PRDebugProbeKind> for DebugProbeKind {
    fn from(value: PRDebugProbeKind) -> Self {
        match value {
            PRDebugProbeKind::SocketAddr(socket) => DebugProbeKind::SocketAddr(socket),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            PRDebugProbeKind::UnixSocketAddr(socket) => DebugProbeKind::UnixSocketAddr(socket),
            PRDebugProbeKind::Usb {
                vendor_id,
                product_id,
                filters,
            } => DebugProbeKind::Usb {
                vendor_id,
                product_id,
                filters: filters.into(),
            },
        }
    }
}

#[allow(clippy::derived_hash_with_manual_eq)]
#[derive(Default, Debug, Clone, Eq, Hash, Deserialize, Serialize, Schema)]
/// Filters for USB devices. Contains the most relevant things that should be unique per-device on
/// a given operating system.
pub struct UsbFilters {
    /// The serial number for a USB device.
    pub serial_number: Option<String>,
    /// The HID interface for a USB device.
    pub hid_interface: Option<u8>,

    #[cfg_attr(
        any(target_os = "linux", target_os = "android"),
        doc = "The path to the USB device in the filesystem."
    )]
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub sysfs_path: Option<PathBuf>,

    #[cfg_attr(target_os = "windows", doc = "The instance ID for a USB device.")]
    #[cfg(target_os = "windows")]
    pub instance_id: Option<String>,
    #[cfg_attr(
        target_os = "windows",
        doc = "The parent instance ID for a USB device."
    )]
    #[cfg(target_os = "windows")]
    pub parent_instance_id: Option<String>,
    #[cfg_attr(target_os = "windows", doc = "The port number for a USB device.")]
    #[cfg(target_os = "windows")]
    pub port_number: Option<u32>,
    #[cfg_attr(target_os = "windows", doc = "The driver for a USB device.")]
    #[cfg(target_os = "windows")]
    pub driver: Option<String>,

    #[cfg_attr(target_os = "macos", doc = "The registry ID for a USB device.")]
    #[cfg(target_os = "macos")]
    pub registry_id: Option<u64>,
    #[cfg_attr(target_os = "macos", doc = "The location ID for a USB device.")]
    #[cfg(target_os = "macos")]
    pub location_id: Option<u32>,
}

impl PartialEq for UsbFilters {
    fn eq(&self, other: &Self) -> bool {
        fn check_eq<T, U>(a: &Option<T>, b: &Option<U>) -> bool
        where
            T: PartialEq<U>,
        {
            match (a, b) {
                (Some(a), Some(b)) => a == b,
                (Some(_), None) => false,
                _ => true,
            }
        }

        if !check_eq(&self.serial_number, &other.serial_number) {
            return false;
        }

        if !check_eq(&self.hid_interface, &other.hid_interface) {
            return false;
        }

        #[cfg(any(target_os = "linux", target_os = "android"))]
        if !check_eq(&self.sysfs_path, &other.sysfs_path) {
            return false;
        }

        #[cfg(target_os = "windows")]
        if !check_eq(&self.instance_id, &other.instance_id)
            || !check_eq(&self.parent_instance_id, &other.parent_instance_id)
            || !check_eq(&self.port_number, &other.port_number)
            || !check_eq(&self.driver, &other.driver)
        {
            return false;
        }

        #[cfg(target_os = "macos")]
        if !check_eq(&self.registry_id, &other.registry_id)
            || !check_eq(&self.location_id, &other.location_id)
        {
            return false;
        }

        true
    }
}

impl std::fmt::Display for UsbFilters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(serial) = self.serial_number.as_ref() {
            write!(f, ":{serial}")?;
        }
        if let Some(iface) = self.hid_interface {
            write!(f, "[{iface}]")?;
        }

        #[cfg(any(target_os = "linux", target_os = "android"))]
        if let Some(path) = self.sysfs_path.as_ref() {
            write!(f, " @ {}", path.display())?;
        }

        #[cfg(any(target_os = "windows", target_os = "macos"))]
        fn helper<T: std::fmt::Display>(
            written: bool,
            f: &mut std::fmt::Formatter<'_>,
            pfx: &str,
            val: Option<&T>,
        ) -> Result<bool, std::fmt::Error> {
            match val {
                Some(val) => {
                    if written {
                        write!(f, ", ")?;
                    }
                    write!(f, "{pfx}={val}")?;
                    Ok(true)
                }
                None => Ok(written),
            }
        }

        #[cfg(target_os = "windows")]
        if self.instance_id.is_some()
            || self.parent_instance_id.is_some()
            || self.port_number.is_some()
            || self.driver.is_some()
        {
            write!(f, "(")?;
            let written = helper(false, f, "ID", self.instance_id.as_ref())?;
            let written = helper(written, f, "PID", self.parent_instance_id.as_ref())?;
            let written = helper(written, f, "PORT", self.port_number.as_ref())?;
            helper(written, f, "DRIVER", self.driver.as_ref())?;
            write!(f, ")")?;
        }

        #[cfg(target_os = "macos")]
        if self.registry_id.is_some() || self.location_id.is_some() {
            write!(f, "(")?;
            let written = helper(false, f, "RID", self.registry_id.as_ref())?;
            helper(written, f, "LID", self.location_id.as_ref())?;
            write!(f, ")")?;
        }

        Ok(())
    }
}

impl From<PRUsbFilters> for UsbFilters {
    fn from(value: PRUsbFilters) -> Self {
        let PRUsbFilters {
            serial_number,
            hid_interface,

            #[cfg(any(target_os = "linux", target_os = "android"))]
            sysfs_path,

            #[cfg(target_os = "windows")]
            instance_id,
            #[cfg(target_os = "windows")]
            parent_instance_id,
            #[cfg(target_os = "windows")]
            port_number,
            #[cfg(target_os = "windows")]
            driver,

            #[cfg(target_os = "macos")]
            registry_id,
            #[cfg(target_os = "macos")]
            location_id,
        } = value;
        UsbFilters {
            serial_number,
            hid_interface,

            #[cfg(any(target_os = "linux", target_os = "android"))]
            sysfs_path,

            #[cfg(target_os = "windows")]
            instance_id,
            #[cfg(target_os = "windows")]
            parent_instance_id,
            #[cfg(target_os = "windows")]
            port_number,
            #[cfg(target_os = "windows")]
            driver,

            #[cfg(target_os = "macos")]
            registry_id,
            #[cfg(target_os = "macos")]
            location_id,
        }
    }
}

impl From<UsbFilters> for PRUsbFilters {
    fn from(value: UsbFilters) -> Self {
        let UsbFilters {
            serial_number,
            hid_interface,

            #[cfg(any(target_os = "linux", target_os = "android"))]
            sysfs_path,

            #[cfg(target_os = "windows")]
            instance_id,
            #[cfg(target_os = "windows")]
            parent_instance_id,
            #[cfg(target_os = "windows")]
            port_number,
            #[cfg(target_os = "windows")]
            driver,

            #[cfg(target_os = "macos")]
            registry_id,
            #[cfg(target_os = "macos")]
            location_id,
        } = value;
        PRUsbFilters {
            serial_number,
            hid_interface,

            #[cfg(any(target_os = "linux", target_os = "android"))]
            sysfs_path,

            #[cfg(target_os = "windows")]
            instance_id,
            #[cfg(target_os = "windows")]
            parent_instance_id,
            #[cfg(target_os = "windows")]
            port_number,
            #[cfg(target_os = "windows")]
            driver,

            #[cfg(target_os = "macos")]
            registry_id,
            #[cfg(target_os = "macos")]
            location_id,
        }
    }
}

#[derive(Serialize, Deserialize, Schema)]
pub struct ListProbesRequest {
    /// Vendor ID filter.
    vid: Option<u16>,
    /// Product ID filter.
    pid: Option<u16>,
}

impl ListProbesRequest {
    pub fn all() -> Self {
        Self {
            vid: None,
            pid: None,
        }
    }
}

pub type ListProbesResponse = RpcResult<Vec<DebugProbeEntry>>;

pub async fn list_probes(
    ctx: &mut RpcContext,
    _header: VarHeader,
    _request: ListProbesRequest,
) -> ListProbesResponse {
    let lister = ctx.lister();
    let probes = lister.list_all().await;

    Ok(probes
        .into_iter()
        .map(DebugProbeEntry::from)
        .collect::<Vec<_>>())
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash, postcard_schema::Schema)]
pub enum DebugProbeSelector {
    /// Selects a USB device based on the inner criteria.
    Usb {
        /// The the USB vendor id of the debug probe to be used.
        vendor_id: u16,
        /// The the USB product id of the debug probe to be used.
        product_id: u16,
        /// Other USB filters.
        filters: UsbFilters,
    },
    /// Selects a network device at the given socket address (IPv4+port, IPv6+port).
    SocketAddr(SocketAddr),
    #[cfg_attr(
        any(target_os = "linux", target_os = "android"),
        doc = "Selects a Unix socket device at the given path."
    )]
    #[cfg(any(target_os = "linux", target_os = "android"))]
    UnixSocketAddr(PathBuf),
}

impl From<DebugProbeKind> for DebugProbeSelector {
    fn from(value: DebugProbeKind) -> Self {
        match value {
            DebugProbeKind::SocketAddr(socket) => DebugProbeSelector::SocketAddr(socket),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            DebugProbeKind::UnixSocketAddr(socket) => DebugProbeSelector::UnixSocketAddr(socket),
            DebugProbeKind::Usb {
                vendor_id,
                product_id,
                filters,
            } => DebugProbeSelector::Usb {
                vendor_id,
                product_id,
                filters,
            },
        }
    }
}

impl From<PRDebugProbeSelector> for DebugProbeSelector {
    fn from(value: PRDebugProbeSelector) -> Self {
        match value {
            PRDebugProbeSelector::SocketAddr(socket) => DebugProbeSelector::SocketAddr(socket),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            PRDebugProbeSelector::UnixSocketAddr(socket) => {
                DebugProbeSelector::UnixSocketAddr(socket)
            }
            PRDebugProbeSelector::Usb {
                vendor_id,
                product_id,
                filters,
            } => DebugProbeSelector::Usb {
                vendor_id,
                product_id,
                filters: filters.into(),
            },
        }
    }
}

impl From<DebugProbeSelector> for PRDebugProbeSelector {
    fn from(value: DebugProbeSelector) -> Self {
        match value {
            DebugProbeSelector::SocketAddr(socket) => PRDebugProbeSelector::SocketAddr(socket),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            DebugProbeSelector::UnixSocketAddr(socket) => {
                PRDebugProbeSelector::UnixSocketAddr(socket)
            }
            DebugProbeSelector::Usb {
                vendor_id,
                product_id,
                filters,
            } => PRDebugProbeSelector::Usb {
                vendor_id,
                product_id,
                filters: filters.into(),
            },
        }
    }
}

#[derive(Serialize, Deserialize, Schema)]
pub struct SelectProbeRequest {
    pub probe: Option<DebugProbeSelector>,
}

#[derive(Serialize, Deserialize, Schema)]
pub enum SelectProbeResult {
    Success(DebugProbeEntry),
    MultipleProbes(Vec<DebugProbeEntry>),
}

pub type SelectProbeResponse = RpcResult<SelectProbeResult>;

pub async fn select_probe(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: SelectProbeRequest,
) -> SelectProbeResponse {
    let lister = ctx.lister();
    let mut list = lister.list(request.probe.map(Into::into).as_ref()).await;

    match list.len() {
        0 => Err(OperationError::NoProbesFound.into()),
        1 => Ok(SelectProbeResult::Success(DebugProbeEntry::from(
            list.swap_remove(0),
        ))),
        _ => Ok(SelectProbeResult::MultipleProbes(
            list.into_iter().map(Into::into).collect(),
        )),
    }
}

#[derive(Serialize, Deserialize, Schema)]
pub enum AttachResult {
    Success(Key<Session>),
    ProbeNotFound,
    FailedToOpenProbe(String),
    ProbeInUse,
}

#[derive(Debug, docsplay::Display, PartialEq, Eq, Clone, Copy, Serialize, Deserialize, Schema)]
pub enum WireProtocol {
    /// JTAG
    Jtag,
    /// SWD
    Swd,
}

impl From<WireProtocol> for probe_rs::probe::WireProtocol {
    fn from(protocol: WireProtocol) -> Self {
        match protocol {
            WireProtocol::Jtag => probe_rs::probe::WireProtocol::Jtag,
            WireProtocol::Swd => probe_rs::probe::WireProtocol::Swd,
        }
    }
}

impl From<probe_rs::probe::WireProtocol> for WireProtocol {
    fn from(protocol: probe_rs::probe::WireProtocol) -> Self {
        match protocol {
            probe_rs::probe::WireProtocol::Jtag => WireProtocol::Jtag,
            probe_rs::probe::WireProtocol::Swd => WireProtocol::Swd,
        }
    }
}

#[derive(Serialize, Deserialize, Schema)]
pub struct AttachRequest {
    pub chip: Option<String>,
    pub protocol: Option<WireProtocol>,
    pub probe: DebugProbeEntry,
    pub speed: Option<u32>,
    pub connect_under_reset: bool,
    pub dry_run: bool,
    pub allow_erase_all: bool,
    pub resume_target: bool,
}

impl From<&AttachRequest> for ProbeOptions {
    fn from(request: &AttachRequest) -> Self {
        ProbeOptions {
            chip: request.chip.clone(),
            chip_description_path: None,
            protocol: request.protocol.map(Into::into),
            non_interactive: true,
            probe: Some(request.probe.selector().into()),
            speed: request.speed,
            connect_under_reset: request.connect_under_reset,
            dry_run: request.dry_run,
            allow_erase_all: request.allow_erase_all,
        }
    }
}

pub type AttachResponse = RpcResult<AttachResult>;

pub async fn attach(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: AttachRequest,
) -> RpcResult<AttachResult> {
    let mut registry = ctx.registry().await;
    let common_options = ProbeOptions::from(&request).load(&mut registry)?;
    let target = common_options.get_target_selector()?;

    let probe = match common_options.attach_probe(&ctx.lister()).await {
        Ok(probe) => probe,
        Err(OperationError::NoProbesFound) => return Ok(AttachResult::ProbeNotFound),
        Err(error) => {
            return Ok(AttachResult::FailedToOpenProbe(format!(
                "{:?}",
                anyhow::anyhow!(error)
            )));
        }
    };

    let mut session = common_options.attach_session(probe, target)?;

    // attach_session halts the target, let's give the user the option
    // to resume it without a roundtrip
    if request.resume_target {
        session.resume_all_cores()?;
    }
    let session_id = ctx.set_session(session, common_options.dry_run()).await;
    Ok(AttachResult::Success(session_id))
}
