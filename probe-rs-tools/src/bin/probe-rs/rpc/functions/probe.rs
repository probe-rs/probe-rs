use std::time::{Duration, Instant};

use postcard_rpc::{header::VarHeader, server::Sender};
use probe_rs::probe::DebugProbeSelector;
use probe_rs_rpc::probe::{
    AttachRequest, AttachResult, DebugProbeEntry, ListProbesResponse, SelectProbeRequest,
    SelectProbeResponse, SelectProbeResult, WireProtocol,
};

use crate::rpc::functions::{RpcContext, RpcSpawnContext, WireTxImpl};
use crate::util::common_options::{
    OPEN_RETRY_INTERVAL, OperationError, ProbeOptions, probe_may_become_available,
};
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

        // Only the outcome of the last attempt is reported, so that the caller
        // learns why the probe is unusable now, not why it was earlier.
        let failure = match attempt {
            AttachAttempt::Attached(session) => {
                let session_id = ctx.set_session(*session, dry_run, lease).await;
                return Ok(AttachResult::Success(session_id));
            }
            AttachAttempt::ProbeGone => None,
            AttachAttempt::Failed(error) => Some(error),
        };

        let broken = failure
            .as_deref()
            .is_some_and(|error| !probe_may_become_available(error));

        let elapsed = start.elapsed();
        if broken || elapsed >= wait_for_probe {
            return Ok(match failure {
                Some(error) => match *error {
                    OperationError::AttachingFailed {
                        source,
                        connect_under_reset,
                    } => AttachResult::TargetAttachFailed {
                        message: source.to_string(),
                        connect_under_reset,
                    },
                    other => {
                        AttachResult::FailedToOpenProbe(format!("{:?}", anyhow::anyhow!(other)))
                    }
                },
                None => AttachResult::ProbeNotFound,
            });
        }

        tokio::select! {
            _ = tokio::time::sleep(OPEN_RETRY_INTERVAL.min(wait_for_probe - elapsed)) => {}
            _ = cancel.cancelled() => return Err("attach cancelled".into()),
        }
    }
}

enum AttachAttempt {
    Attached(Box<probe_rs::Session>),
    /// The probe is not in the probe list. A probe that another process holds
    /// can drop out of the list, so this is not necessarily permanent.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::functions::RpcApp;
    use crate::rpc::probe_broker::ProbeBroker;
    use probe_rs::architecture::arm::FullyQualifiedApAddress;
    use probe_rs::integration::{FakeProbe, Operation, ProbeLister};
    use probe_rs::probe::{
        DebugProbe, DebugProbeError, DebugProbeInfo, Probe, ProbeCreationError, ProbeFactory,
        list::ProbeListItem,
    };
    use probe_rs_rpc_client::RpcClient;
    use std::fmt::Display;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const TEST_CHIP_NAME: &str = "nRF52833_xxAA";

    fn program_binary() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../probe-rs-debug/tests/debug-unwind-tests/nRF52833_xxAA_full_unwind.elf")
    }

    #[derive(Debug)]
    struct MockProbeFactory;

    impl Display for MockProbeFactory {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Mocked Probe")
        }
    }

    impl ProbeFactory for MockProbeFactory {
        fn open(
            &self,
            _selector: &DebugProbeSelector,
        ) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
            unreachable!("BusyLister opens FakeProbe directly")
        }

        fn list_probes(&self) -> Vec<ProbeListItem> {
            Vec::new()
        }
    }

    /// A probe that another process holds. `open` fails until `free_after`
    /// attempts have been made, and while it is held the probe is missing from
    /// the probe list if `listed_while_busy` is false.
    struct BusyLister {
        info: DebugProbeInfo,
        probe: Mutex<Option<FakeProbe>>,
        attempts: AtomicUsize,
        free_after: usize,
        listed_while_busy: bool,
        faulty: bool,
    }

    impl BusyLister {
        fn new(free_after: usize) -> Self {
            let probe = FakeProbe::with_mocked_core_and_binary(program_binary().as_path());
            probe.expect_operation(Operation::ReadRawApRegister {
                ap: FullyQualifiedApAddress::v1_with_default_dp(1),
                address: 0xC,
                result: 1,
            });

            Self {
                info: DebugProbeInfo::new(
                    "Mock probe",
                    0x12,
                    0x23,
                    Some("busy_serial".to_owned()),
                    &MockProbeFactory,
                    None,
                    false,
                ),
                probe: Mutex::new(Some(probe)),
                attempts: AtomicUsize::new(0),
                free_after,
                listed_while_busy: true,
                faulty: false,
            }
        }

        /// The probe drops out of the probe list while another process holds it.
        fn hidden_while_busy(mut self) -> Self {
            self.listed_while_busy = false;
            self
        }

        /// The probe answers the open, then faults.
        fn faulty(mut self) -> Self {
            self.faulty = true;
            self
        }

        fn attempts(&self) -> usize {
            self.attempts.load(Ordering::SeqCst)
        }

        fn busy(&self) -> bool {
            self.attempts() < self.free_after
        }
    }

    impl std::fmt::Debug for BusyLister {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("BusyLister")
        }
    }

    impl ProbeLister for BusyLister {
        fn open(&self, _selector: &DebugProbeSelector) -> Result<Probe, DebugProbeError> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            if self.faulty {
                return Err(DebugProbeError::ProbeCouldNotBeCreated(
                    ProbeCreationError::Usb(std::io::Error::other(
                        "hardware fault or protocol violation",
                    )),
                ));
            }
            if attempt < self.free_after {
                return Err(DebugProbeError::ProbeCouldNotBeCreated(
                    ProbeCreationError::CouldNotOpen,
                ));
            }

            match self.probe.lock().unwrap().take() {
                Some(probe) => Ok(Probe::from_specific_probe(Box::new(probe))),
                None => Err(DebugProbeError::ProbeCouldNotBeCreated(
                    ProbeCreationError::CouldNotOpen,
                )),
            }
        }

        fn list_with_access(&self, _selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
            if self.busy() && !self.listed_while_busy {
                return Vec::new();
            }
            vec![ProbeListItem::accessible(self.info.clone())]
        }
    }

    async fn attach(lister: Arc<BusyLister>, wait_for_probe: Option<Duration>) -> AttachResult {
        let probe =
            convert::to_wire_debug_probe_entry(ProbeListItem::accessible(lister.info.clone()));

        let (mut server, tx, rx) = RpcApp::create_server_with_lister(
            16,
            lister as Arc<dyn ProbeLister + Send + Sync>,
            Arc::new(ProbeBroker::new()),
        );
        let handle = tokio::spawn(async move { server.run().await });
        let client = RpcClient::new_local_from_wire(tx, rx);

        let result = client
            .attach_probe(AttachRequest {
                chip: Some(TEST_CHIP_NAME.to_owned()),
                protocol: None,
                probe,
                speed: None,
                connect_under_reset: false,
                dry_run: false,
                allow_erase_all: false,
                resume_target: false,
                wait_for_probe,
            })
            .await;

        drop(client);
        _ = handle.await;

        result.unwrap()
    }

    #[tokio::test]
    async fn busy_probe_fails_immediately_without_a_timeout() {
        let lister = Arc::new(BusyLister::new(usize::MAX));

        let result = attach(lister.clone(), None).await;

        assert!(matches!(result, AttachResult::FailedToOpenProbe(_)));
        assert_eq!(lister.attempts(), 1);
    }

    #[tokio::test]
    async fn busy_probe_is_retried_until_it_can_be_opened() {
        let lister = Arc::new(BusyLister::new(2));

        let result = attach(lister.clone(), Some(Duration::from_secs(30))).await;

        assert!(matches!(result, AttachResult::Success(_)));
        assert_eq!(lister.attempts(), 3);
    }

    /// A probe that another process holds can be missing from the probe list.
    #[tokio::test]
    async fn unlisted_probe_is_retried_until_it_reappears() {
        let lister = Arc::new(BusyLister::new(2).hidden_while_busy());

        let result = attach(lister.clone(), Some(Duration::from_secs(30))).await;

        assert!(matches!(result, AttachResult::Success(_)));
    }

    /// The reason the probe could not be opened is more useful than a bare
    /// "the probe is in use".
    #[tokio::test]
    async fn expired_wait_reports_why_the_probe_could_not_be_opened() {
        let lister = Arc::new(BusyLister::new(usize::MAX));

        let result = attach(lister.clone(), Some(Duration::from_millis(1500))).await;

        let AttachResult::FailedToOpenProbe(error) = result else {
            panic!("expected the open error");
        };
        assert!(
            error.contains("could not be opened"),
            "unexpected error: {error}"
        );
        assert!(lister.attempts() > 1, "the probe was tried only once");
    }

    /// A probe that faults is broken now and stays broken, so the wait cannot
    /// help. Retrying it until the attach timeout only delays the report.
    #[tokio::test]
    async fn faulting_probe_is_reported_without_waiting_out_the_timeout() {
        let lister = Arc::new(BusyLister::new(usize::MAX).faulty());

        let result = attach(lister.clone(), Some(Duration::from_secs(600))).await;

        let AttachResult::FailedToOpenProbe(error) = result else {
            panic!("expected the open error");
        };
        assert!(
            error.contains("hardware fault or protocol violation"),
            "unexpected error: {error}"
        );
        assert_eq!(lister.attempts(), 1);
    }
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
                // `attach_impl` runs the wait, so that it can also retry a
                // probe that has dropped out of the probe list, and so that the
                // client can cancel it.
                attach_timeout: None,
            }
        }
    }
}
