use std::time::Duration;

use postcard_rpc::{header::VarHeader, server::Sender};
use probe_rs::probe::{DebugProbeSelector, Probe};
use probe_rs_rpc::reset::{
    ResetCoreAndHaltRequest, ResetCoreRequest, ResetMethod, ResetProbeRequest, ResetProbeResponse,
};

use crate::rpc::functions::probe::convert::{from_wire_debug_probe_selector, from_wire_protocol};
use crate::rpc::functions::probe::{OpenFailure, OpenResult, open_probe, open_with_retry};
use crate::rpc::functions::{RpcContext, RpcSpawnContext, WireTxImpl, convert::lift};
use crate::util::common_options::{LoadedProbeOptions, OperationError, ProbeOptions};
use probe_rs_rpc::core_ops::WireCoreInformation;
use probe_rs_rpc::{NoResponse, ResetProbeEndpoint, RpcResult};

/// How long a pin reset holds the reset line low.
const RESET_HOLD_TIME: Duration = Duration::from_millis(100);

pub async fn reset(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ResetCoreRequest,
) -> NoResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut core = lift(session.core(request.core as usize))?;
    lift(core.reset())?;
    Ok(())
}

pub async fn reset_and_halt(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ResetCoreAndHaltRequest,
) -> RpcResult<WireCoreInformation> {
    let mut session = ctx.session(request.sessid).await;
    let mut core = lift(session.core(request.core as usize))?;
    let info = lift(core.reset_and_halt(request.timeout))?;
    Ok(crate::rpc::functions::core_ops::convert::to_wire_core_information(info))
}

pub async fn reset_probe(
    ctx: RpcSpawnContext,
    header: VarHeader,
    request: ResetProbeRequest,
    sender: Sender<WireTxImpl>,
) {
    let resp = reset_probe_impl(ctx, request).await;

    sender
        .reply::<ResetProbeEndpoint>(header.seq_no, &resp)
        .await
        .unwrap();
}

async fn reset_probe_impl(ctx: RpcSpawnContext, request: ResetProbeRequest) -> ResetProbeResponse {
    let selector = from_wire_debug_probe_selector(request.probe.selector());
    let probe_config = ctx.probe_broker().probe_configs().resolve(&selector);
    let pin_reset = wants_pin_reset(request.method, probe_config.reset_line_connected)?;
    if !pin_reset {
        tracing::info!("Resetting the core");
        return Ok(false);
    }
    tracing::info!("Pulsing the reset line of the probe");

    let wait_for_probe = request.wait_for_probe.unwrap_or(Duration::ZERO);
    let probe_options = ProbeOptions {
        chip: request.chip,
        chip_description_path: None,
        protocol: request.protocol.map(from_wire_protocol),
        non_interactive: true,
        probe: Some(selector.clone()),
        speed: None,
        connect_under_reset: false,
        cycle_power: false,
        allow_erase_all: false,
        attach_timeout: request.wait_for_probe,
    };

    let attempt = {
        let ctx = ctx.clone();
        let selector = selector.clone();
        move || reset_once(&ctx, probe_options.clone(), &selector)
    };

    match open_with_retry(&ctx, &selector, wait_for_probe, "reset", attempt).await? {
        OpenResult::Opened((), _lease) => Ok(true),
        OpenResult::InUse => Err("The probe is in use".into()),
        OpenResult::Failed(error) => Err((*error).into()),
        OpenResult::Gone => Err(OperationError::NoProbesFound.into()),
    }
}

fn reset_once(
    ctx: &RpcSpawnContext,
    probe_options: ProbeOptions,
    selector: &DebugProbeSelector,
) -> RpcResult<Result<(), OpenFailure>> {
    let lister = ctx.lister();
    let mut registry = ctx.registry_blocking();
    let loaded = probe_options.load(&mut registry)?;
    let mut probe = match open_probe(&lister, &loaded, selector) {
        Ok(probe) => probe,
        Err(failure) => return Ok(Err(failure)),
    };

    pulse_reset(&mut probe)
        .map_err(|error| format!("The probe cannot pulse the reset line: {error}"))?;
    // A pin reset keeps the debug state of the cores, for example a halt that an earlier
    // session left.
    if let Err(error) = clear_debug_state(&loaded, probe) {
        tracing::warn!("Could not clear the debug state after the reset: {error:?}");
    }
    Ok(Ok(()))
}

/// Ending a session clears the debug state of the cores.
fn clear_debug_state(options: &LoadedProbeOptions, probe: Probe) -> anyhow::Result<()> {
    let target = options.get_target_selector()?;
    let mut session = options.attach_session(probe, target)?;
    session.resume_all_cores()?;
    Ok(())
}

fn pulse_reset(probe: &mut Probe) -> anyhow::Result<()> {
    // Some probes drive the reset line only after they start the wire protocol.
    probe.attach_to_unspecified()?;
    probe.target_reset_assert()?;
    std::thread::sleep(RESET_HOLD_TIME);
    probe.target_reset_deassert()?;
    probe.detach()?;
    Ok(())
}

/// Whether to pulse the reset line. `method` overrides the probe settings, but a line that the
/// settings call unconnected cannot be pulsed.
fn wants_pin_reset(
    method: Option<ResetMethod>,
    reset_line_connected: Option<bool>,
) -> Result<bool, &'static str> {
    match (method, reset_line_connected) {
        (Some(ResetMethod::Pin), Some(false)) => {
            Err("`reset_line_connected` is false in the probe settings")
        }
        (Some(method), _) => Ok(method == ResetMethod::Pin),
        (None, connected) => Ok(connected == Some(true)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_decide_without_a_method() {
        assert_eq!(wants_pin_reset(None, None), Ok(false));
        assert_eq!(wants_pin_reset(None, Some(false)), Ok(false));
        assert_eq!(wants_pin_reset(None, Some(true)), Ok(true));
    }

    #[test]
    fn the_method_overrides_the_settings() {
        for connected in [None, Some(true)] {
            assert_eq!(wants_pin_reset(Some(ResetMethod::Pin), connected), Ok(true));
        }
        for connected in [None, Some(false), Some(true)] {
            assert_eq!(
                wants_pin_reset(Some(ResetMethod::Core), connected),
                Ok(false)
            );
        }
    }

    #[test]
    fn pin_is_refused_for_an_unconnected_line() {
        assert!(wants_pin_reset(Some(ResetMethod::Pin), Some(false)).is_err());
    }
}
