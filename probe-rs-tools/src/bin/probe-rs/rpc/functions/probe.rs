use postcard_rpc::header::VarHeader;
use probe_rs_rpc::probe::{
    AttachRequest, AttachResult, DebugProbeEntry, DebugProbeSelector, ListProbesResponse,
    SelectProbeRequest, SelectProbeResponse, SelectProbeResult, WireProtocol,
};

use crate::rpc::functions::{RpcContext, convert::lift};
use crate::util::common_options::{OperationError, ProbeOptions};
use probe_rs_rpc::RpcResult;

pub fn list_probes(ctx: &mut RpcContext, _header: VarHeader, _request: ()) -> ListProbesResponse {
    let lister = ctx.lister();
    let probes = lister.list_all_with_access();

    Ok(probes
        .into_iter()
        .map(convert::to_wire_debug_probe_entry)
        .collect::<Vec<_>>())
}

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

    let mut list = lister.list_with_access(
        request
            .probe
            .as_ref()
            .map(|sel| convert::from_wire_debug_probe_selector(sel.clone()))
            .as_ref(),
    );

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
            convert::to_wire_debug_probe_entry(list.swap_remove(0)),
        ))),
        _ => Ok(SelectProbeResult::MultipleProbes(
            list.into_iter()
                .map(|e| with_interface(convert::to_wire_debug_probe_entry(e)))
                .collect(),
        )),
    }
}

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
        lift(session.resume_all_cores())?;
    }
    let session_id = ctx.set_session(session, common_options.dry_run()).await;
    Ok(AttachResult::Success(session_id))
}

pub(crate) mod convert {
    use super::{AttachRequest, DebugProbeEntry, DebugProbeSelector, WireProtocol};
    use crate::util::common_options::ProbeOptions;
    use probe_rs::probe::list::{Accessibility, ProbeListItem};

    pub(crate) fn to_wire_debug_probe_entry(item: ProbeListItem) -> DebugProbeEntry {
        let inaccessible = item.accessibility == Accessibility::PermissionDenied;
        let probe = item.info;
        DebugProbeEntry {
            probe_type: probe.probe_type(),
            inaccessible,
            identifier: probe.identifier,
            vendor_id: probe.vendor_id,
            product_id: probe.product_id,
            serial_number: probe.serial_number.unwrap_or_default(),
            interface: probe.interface,
        }
    }

    pub(crate) fn from_wire_protocol(protocol: WireProtocol) -> probe_rs::probe::WireProtocol {
        match protocol {
            WireProtocol::Jtag => probe_rs::probe::WireProtocol::Jtag,
            WireProtocol::Swd => probe_rs::probe::WireProtocol::Swd,
        }
    }

    pub(crate) fn to_wire_protocol(protocol: probe_rs::probe::WireProtocol) -> WireProtocol {
        match protocol {
            probe_rs::probe::WireProtocol::Jtag => WireProtocol::Jtag,
            probe_rs::probe::WireProtocol::Swd => WireProtocol::Swd,
        }
    }

    pub(crate) fn to_wire_debug_probe_selector(
        selector: probe_rs::probe::DebugProbeSelector,
    ) -> DebugProbeSelector {
        DebugProbeSelector {
            vendor_id: selector.vendor_id,
            product_id: selector.product_id,
            serial_number: selector.serial_number,
            interface: selector.interface,
        }
    }

    pub(crate) fn from_wire_debug_probe_selector(
        selector: DebugProbeSelector,
    ) -> probe_rs::probe::DebugProbeSelector {
        probe_rs::probe::DebugProbeSelector {
            vendor_id: selector.vendor_id,
            product_id: selector.product_id,
            serial_number: selector.serial_number,
            interface: selector.interface,
        }
    }

    impl From<&AttachRequest> for ProbeOptions {
        fn from(request: &AttachRequest) -> Self {
            ProbeOptions {
                chip: request.chip.clone(),
                chip_description_path: None,
                protocol: request.protocol.map(from_wire_protocol),
                non_interactive: true,
                probe: Some(from_wire_debug_probe_selector(request.probe.selector())),
                speed: request.speed,
                connect_under_reset: request.connect_under_reset,
                cycle_power: false,
                dry_run: request.dry_run,
                allow_erase_all: request.allow_erase_all,
            }
        }
    }
}
