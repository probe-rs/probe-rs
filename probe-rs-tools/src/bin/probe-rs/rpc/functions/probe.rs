use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::{Session, probe::DebugProbeInfo};
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
    /// The USB vendor ID of the debug probe.
    pub vendor_id: u16,
    /// The USB product ID of the debug probe.
    pub product_id: u16,
    /// The interface of the debug probe.
    pub interface: Option<u8>,
    /// The serial number of the debug probe.
    pub serial_number: String,

    pub probe_type: String,
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

        write!(f, ":{} ({})", self.serial_number, self.probe_type)
    }
}

impl From<DebugProbeInfo> for DebugProbeEntry {
    fn from(probe: DebugProbeInfo) -> DebugProbeEntry {
        DebugProbeEntry {
            probe_type: probe.probe_type(),
            identifier: probe.identifier,
            vendor_id: probe.vendor_id,
            product_id: probe.product_id,
            serial_number: probe.serial_number.unwrap_or_default(),
            interface: probe.interface,
        }
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

    // Capture the requested interface before consuming the selector.
    // Some probe types (e.g. FTDI multi-channel) list one entry per USB device
    // without per-channel DebugProbeInfo entries; the channel is resolved at
    // open() time via the selector. We must propagate the interface from the
    // original selector into the returned DebugProbeEntry so that the subsequent
    // attach() call opens the correct channel.
    let requested_interface = request.probe.as_ref().and_then(|s| s.interface);

    let mut list = lister.list(request.probe.map(|sel| sel.into()).as_ref());

    // If the probe entry does not carry an interface (common for FTDI probes)
    // but the caller requested one, copy it from the original selector.
    let with_interface = |mut entry: DebugProbeEntry| {
        if entry.interface.is_none() {
            entry.interface = requested_interface;
        }
        entry
    };

    match list.len() {
        0 => Err(OperationError::NoProbesFound.into()),
        1 => Ok(SelectProbeResult::Success(with_interface(
            DebugProbeEntry::from(list.swap_remove(0)),
        ))),
        _ => Ok(SelectProbeResult::MultipleProbes(
            list.into_iter()
                .map(|e| with_interface(DebugProbeEntry::from(e)))
                .collect(),
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

impl From<probe_rs::probe::DebugProbeSelector> for DebugProbeSelector {
    fn from(selector: probe_rs::probe::DebugProbeSelector) -> Self {
        Self {
            vendor_id: selector.vendor_id,
            product_id: selector.product_id,
            serial_number: selector.serial_number,
            interface: selector.interface,
        }
    }
}

impl From<DebugProbeSelector> for probe_rs::probe::DebugProbeSelector {
    fn from(selector: DebugProbeSelector) -> Self {
        Self {
            vendor_id: selector.vendor_id,
            product_id: selector.product_id,
            serial_number: selector.serial_number,
            interface: selector.interface,
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
            cycle_power: false,
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

    let probe = match common_options.attach_probe(&ctx.lister()) {
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
