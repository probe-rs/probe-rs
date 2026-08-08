use std::fmt::Display;

use std::time::Duration;

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::{Key, RpcResult, Session};

// Separate from DebugProbeInfo because we can't serialize a &dyn ProbeFactory
#[derive(Debug, Serialize, Deserialize, Clone, Schema)]
pub struct DebugProbeEntry {
    /// The name of the debug probe.
    pub identifier: String,
    /// The USB vendor ID of the debug probe.
    pub vendor_id: u16,
    /// The USB product ID of the debug probe.
    pub product_id: u16,
    /// The interface of the debug probe.
    pub interface: Option<u8>,
    /// The serial number of the debug probe.
    pub serial_number: String,

    pub probe_type: String,

    /// The probe was found but the current user cannot access its device
    /// (e.g. a missing udev rule on Linux).
    pub inaccessible: bool,
}

impl Display for DebugProbeEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} -- {:04x}:{:04x}",
            self.identifier, self.vendor_id, self.product_id,
        )?;

        if let Some(interface) = self.interface {
            write!(f, "-{}", interface)?;
        }

        write!(f, ":{} ({})", self.serial_number, self.probe_type)?;

        Ok(())
    }
}

impl DebugProbeEntry {
    pub fn selector(&self) -> DebugProbeSelector {
        DebugProbeSelector {
            vendor_id: self.vendor_id,
            product_id: self.product_id,
            serial_number: Some(self.serial_number.clone()),
            interface: self.interface,
        }
    }
}

pub type ListProbesResponse = RpcResult<Vec<DebugProbeEntry>>;

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

#[derive(Serialize, Deserialize, Schema)]
pub enum AttachResult {
    Success(Key<Session>),
    ProbeNotFound,
    FailedToOpenProbe(String),
    ProbeInUse,
    /// The probe opened, but attaching to the chip failed.
    TargetAttachFailed {
        message: String,
        connect_under_reset: bool,
    },
}

#[derive(Debug, docsplay::Display, PartialEq, Eq, Clone, Copy, Serialize, Deserialize, Schema)]
pub enum WireProtocol {
    /// JTAG
    Jtag,
    /// SWD
    Swd,
}

#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct DebugProbeSelector {
    /// The the USB vendor id of the debug probe to be used.
    pub vendor_id: u16,
    /// The the USB product id of the debug probe to be used.
    pub product_id: u16,
    /// The the interface of the debug probe to be used.
    pub interface: Option<u8>,
    /// The the serial number of the debug probe to be used.
    pub serial_number: Option<String>,
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
    pub wait_for_probe: Option<Duration>,
}

pub type AttachResponse = RpcResult<AttachResult>;
