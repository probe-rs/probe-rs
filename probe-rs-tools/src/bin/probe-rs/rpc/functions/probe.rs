use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::{probe::DebugProbeInfo, Session};
use serde::{Deserialize, Serialize};

use crate::{
    rpc::{
        functions::{RpcContext, RpcResult},
        Key,
    },
    util::common_options::{OperationError, ProbeOptions},
};

use std::fmt::Display;

// Separate from DebugProbeInfo because we can't serialize a &dyn ProbeFactory
#[derive(Serialize, Deserialize, Clone, Schema)]
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

pub fn list_probes(
    ctx: &mut RpcContext,
    _header: VarHeader,
    _request: ListProbesRequest,
) -> ListProbesResponse {
    let lister = ctx.lister();
    let probes = lister.list_all();

    Ok(probes
        .into_iter()
        .map(DebugProbeEntry::from)
        .collect::<Vec<_>>())
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
    let mut list = lister.list_all();

    if let Some(selector) = request.probe {
        list.retain(|info| selector.matches(info));
    }

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

#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct DebugProbeSelector {
    /// The the USB vendor id of the debug probe to be used.
    pub vendor_id: u16,
    /// The the USB product id of the debug probe to be used.
    pub product_id: u16,
    /// The the serial number of the debug probe to be used.
    pub serial_number: Option<String>,
}

impl DebugProbeSelector {
    pub fn matches(&self, probe: &DebugProbeInfo) -> bool {
        probe.vendor_id == self.vendor_id
            && probe.product_id == self.product_id
            && self
                .serial_number
                .as_ref()
                .map(|s| {
                    if let Some(ref serial_number) = probe.serial_number {
                        serial_number == s
                    } else {
                        // Match probes without serial number when the
                        // selector has a third, empty part ("VID:PID:")
                        s.is_empty()
                    }
                })
                .unwrap_or(true)
    }
}

impl From<probe_rs::probe::DebugProbeSelector> for DebugProbeSelector {
    fn from(selector: probe_rs::probe::DebugProbeSelector) -> Self {
        Self {
            vendor_id: selector.vendor_id,
            product_id: selector.product_id,
            serial_number: selector.serial_number,
        }
    }
}

impl From<DebugProbeSelector> for probe_rs::probe::DebugProbeSelector {
    fn from(selector: DebugProbeSelector) -> Self {
        Self {
            vendor_id: selector.vendor_id,
            product_id: selector.product_id,
            serial_number: selector.serial_number,
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
    let common_options = ProbeOptions::from(&request).load()?;
    let target = common_options.get_target_selector()?;

    let Ok(probe) = common_options.attach_probe(&ctx.lister()) else {
        return Ok(AttachResult::ProbeNotFound);
    };

    let mut session = common_options.attach_session(probe, target)?;
    if request.resume_target {
        session.resume_all_cores()?;
    }
    let session_id = ctx.set_session(session, common_options.dry_run()).await;
    Ok(AttachResult::Success(session_id))
}