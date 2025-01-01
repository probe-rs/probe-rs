use std::fmt::Display;

use probe_rs::probe::{list::Lister, DebugProbeInfo, DebugProbeSelector};
use serde::{Deserialize, Serialize};

use crate::cmd::remote::{functions::RemoteFunctions, LocalSession};

// Separate from DebugProbeInfo because we can't serialize a &dyn ProbeFactory
#[derive(Serialize, Deserialize, Clone)]
pub struct DebugProbeEntry {
    /// The name of the debug probe.
    pub identifier: String,
    /// The USB vendor ID of the debug probe.
    pub vendor_id: u16,
    /// The USB product ID of the debug probe.
    pub product_id: u16,
    /// The serial number of the debug probe.
    pub serial_number: Option<String>,

    pub probe_type: String,
}

impl Display for DebugProbeEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} -- {:04x}:{:04x}:{} ({})",
            self.identifier,
            self.vendor_id,
            self.product_id,
            self.serial_number.as_deref().unwrap_or(""),
            self.probe_type,
        )
    }
}

impl From<DebugProbeInfo> for DebugProbeEntry {
    fn from(probe: DebugProbeInfo) -> DebugProbeEntry {
        DebugProbeEntry {
            identifier: probe.identifier.clone(),
            vendor_id: probe.vendor_id,
            product_id: probe.product_id,
            serial_number: probe.serial_number.clone(),
            probe_type: probe.probe_type(),
        }
    }
}

impl DebugProbeEntry {
    pub fn selector(&self) -> DebugProbeSelector {
        DebugProbeSelector {
            vendor_id: self.vendor_id,
            product_id: self.product_id,
            serial_number: self.serial_number.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct ListProbes {
    vid: Option<u16>,
    pid: Option<u16>,
}

impl ListProbes {
    pub fn new() -> Self {
        Self {
            vid: None,
            pid: None,
        }
    }
}

impl super::RemoteFunction for ListProbes {
    type Result = Vec<DebugProbeEntry>;

    async fn run(self, _iface: &mut LocalSession) -> Self::Result {
        let lister = Lister::new();
        let probes = lister.list_all();

        probes
            .into_iter()
            .map(DebugProbeEntry::from)
            .collect::<Vec<_>>()
    }
}

impl From<ListProbes> for RemoteFunctions {
    fn from(func: ListProbes) -> Self {
        RemoteFunctions::ListProbes(func)
    }
}
