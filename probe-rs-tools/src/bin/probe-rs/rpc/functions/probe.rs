use std::time::{Duration, Instant};

use postcard_rpc::{header::VarHeader, server::Sender};
use probe_rs::probe::DebugProbeSelector;
use probe_rs_rpc::probe::{
    AttachRequest, AttachResult, DebugProbeEntry, ListProbesResponse, SelectProbeRequest,
    SelectProbeResponse, SelectProbeResult, WireProtocol,
};

use crate::rpc::functions::{RpcContext, RpcSpawnContext, WireTxImpl};
use crate::util::common_options::{OperationError, ProbeOptions};
use probe_rs_rpc::{AttachEndpoint, RpcResult};

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

/// How long to wait before another attempt at a probe that is held by a
/// process this server does not know about.
const OPEN_RETRY_INTERVAL: Duration = Duration::from_secs(1);

pub async fn attach(
    ctx: RpcSpawnContext,
    header: VarHeader,
    request: AttachRequest,
    sender: Sender<WireTxImpl>,
) {
    let resp = attach_impl(ctx, request).await;

    sender
        .reply::<AttachEndpoint>(header.seq_no, &resp)
        .await
        .unwrap();
}

async fn attach_impl(ctx: RpcSpawnContext, request: AttachRequest) -> RpcResult<AttachResult> {
    let probe_options = ProbeOptions::from(&request);
    let dry_run = probe_options.dry_run;
    let selector = convert::from_wire_debug_probe_selector(request.probe.selector());
    let cancel = ctx.cancellation_token();

    let wait_for_probe = request.wait_for_probe.unwrap_or(Duration::ZERO);
    // A dry run drives a `FakeProbe` and claims no device.
    let lease = if dry_run {
        None
    } else {
        let acquire = ctx.probe_broker().acquire(selector.clone());

        tokio::select! {
            granted = tokio::time::timeout(wait_for_probe, acquire) => match granted {
                Ok(lease) => Some(lease),
                Err(_) => return Ok(AttachResult::ProbeInUse),
            },
            _ = cancel.cancelled() => return Err("attach cancelled".into()),
        }
    };

    let start = Instant::now();

    loop {
        let attempt = {
            let ctx = ctx.clone();
            let probe_options = probe_options.clone();
            let selector = selector.clone();

            tokio::task::spawn_blocking(move || {
                attach_once(ctx, probe_options, selector, request.resume_target)
            })
            .await
            .unwrap()?
        };

        let error = match attempt {
            AttachAttempt::Attached(session) => {
                let session_id = ctx.set_session(*session, dry_run, lease).await;
                return Ok(AttachResult::Success(session_id));
            }
            AttachAttempt::ProbeGone => return Ok(AttachResult::ProbeNotFound),
            AttachAttempt::Failed(error) => error,
        };

        if start.elapsed() >= wait_for_probe {
            // Only a caller that asked to wait can conclude that the probe is
            // busy. Without a timeout the open error is the better diagnostic:
            // it also covers a probe the user has no permission to open.
            if request.wait_for_probe.is_some() {
                return Ok(AttachResult::ProbeInUse);
            }
            return Ok(AttachResult::FailedToOpenProbe(format!(
                "{:?}",
                anyhow::anyhow!(error)
            )));
        }

        tokio::select! {
            _ = tokio::time::sleep(OPEN_RETRY_INTERVAL) => {}
            _ = cancel.cancelled() => return Err("attach cancelled".into()),
        }
    }
}

enum AttachAttempt {
    Attached(Box<probe_rs::Session>),
    /// The probe is no longer connected, so waiting for it is pointless.
    ProbeGone,
    Failed(Box<OperationError>),
}

fn attach_once(
    ctx: RpcSpawnContext,
    probe_options: ProbeOptions,
    selector: DebugProbeSelector,
    resume_target: bool,
) -> RpcResult<AttachAttempt> {
    use crate::rpc::functions::convert::lift;

    let lister = ctx.lister();
    let mut registry = ctx.registry_blocking();
    let loaded = probe_options.load(&mut registry)?;
    let target = loaded.get_target_selector()?;

    let probe = match loaded.attach_probe(&lister) {
        Ok(probe) => probe,
        Err(OperationError::NoProbesFound) => return Ok(AttachAttempt::ProbeGone),
        Err(error) => {
            if lister.list_with_access(Some(&selector)).is_empty() {
                return Ok(AttachAttempt::ProbeGone);
            }
            return Ok(AttachAttempt::Failed(Box::new(error)));
        }
    };

    let mut session = loaded.attach_session(probe, target)?;
    if resume_target {
        lift(session.resume_all_cores())?;
    }

    Ok(AttachAttempt::Attached(Box::new(session)))
}

pub(crate) mod convert {
    use super::{AttachRequest, DebugProbeEntry, WireProtocol};
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
    ) -> probe_rs_rpc::probe::DebugProbeSelector {
        probe_rs_rpc::probe::DebugProbeSelector {
            vendor_id: selector.vendor_id,
            product_id: selector.product_id,
            serial_number: selector.serial_number,
            interface: selector.interface,
        }
    }

    pub(crate) fn from_wire_debug_probe_selector(
        selector: probe_rs_rpc::probe::DebugProbeSelector,
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
                attach_timeout: None,
            }
        }
    }
}
