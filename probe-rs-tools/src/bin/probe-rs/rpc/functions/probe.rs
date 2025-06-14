use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::{
    Session,
    probe::{DebugProbeInfo, DebugProbeKind, DebugProbeSelector},
};
use serde::{Deserialize, Serialize};

use crate::{
    rpc::{
        Key,
        functions::{RpcContext, RpcResult},
    },
    util::common_options::{OperationError, ProbeOptions},
};

use std::fmt::Display;

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
            kind: probe.kind,
        }
    }
}

impl DebugProbeEntry {
    pub fn selector(&self) -> DebugProbeSelector {
        self.kind.clone().into()
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
    let mut list = lister.list(request.probe.as_ref()).await;

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
            probe: Some(request.probe.selector()),
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

    let Ok(probe) = common_options.attach_probe(&ctx.lister()).await else {
        return Ok(AttachResult::ProbeNotFound);
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
