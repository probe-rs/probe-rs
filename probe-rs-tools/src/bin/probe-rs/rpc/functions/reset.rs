use std::time::Duration;

use anyhow::Context as _;
use postcard_rpc::{header::VarHeader, server::Sender};
use probe_rs::config::TargetSelector;
use probe_rs::probe::{DebugProbeSelector, Probe};
use probe_rs_rpc::probe::AttachResult;
use probe_rs_rpc::reset::{
    ResetCoreAndHaltRequest, ResetCoreRequest, ResetMethod, ResetTargetRequest, ResetTargetResponse,
};

use crate::rpc::functions::probe::convert::from_wire_debug_probe_selector;
use crate::rpc::functions::probe::{OpenFailure, open_probe, open_with_retry};
use crate::rpc::functions::{RpcContext, RpcSpawnContext, WireTxImpl, convert::lift};
use crate::util::common_options::{LoadedProbeOptions, ProbeOptions};
use probe_rs_rpc::core_ops::WireCoreInformation;
use probe_rs_rpc::{NoResponse, ResetTargetEndpoint, RpcResult};

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

pub async fn reset_target(
    ctx: RpcSpawnContext,
    header: VarHeader,
    request: ResetTargetRequest,
    sender: Sender<WireTxImpl>,
) {
    let resp = reset_target_impl(ctx, request).await;

    sender
        .reply::<ResetTargetEndpoint>(header.seq_no, &resp)
        .await
        .unwrap();
}

async fn reset_target_impl(
    ctx: RpcSpawnContext,
    request: ResetTargetRequest,
) -> ResetTargetResponse {
    let selector = from_wire_debug_probe_selector(request.attach.probe.selector());
    let probe_config = ctx.probe_broker().probe_configs().resolve(&selector);
    let method = reset_method(request.method, probe_config.reset_line_connected)?;
    match method {
        ResetMethod::Pin => tracing::info!("Pulsing the reset line of the probe"),
        ResetMethod::Core => tracing::info!("Resetting the core"),
    }
    if method == ResetMethod::Pin && request.attach.connect_under_reset {
        tracing::warn!("A pin reset ignores connect under reset");
    }

    let wait_for_probe = request.attach.wait_for_probe.unwrap_or(Duration::ZERO);
    let probe_options = ProbeOptions::from(&request.attach);
    let core = request.core as usize;

    let attempt = {
        let ctx = ctx.clone();
        let selector = selector.clone();
        move || reset_once(&ctx, probe_options.clone(), &selector, method, core)
    };

    let opened = open_with_retry(&ctx, &selector, wait_for_probe, "reset", attempt).await?;
    Ok(match opened.into_opened() {
        Ok(((), _lease)) => AttachResult::Success(()),
        Err(failure) => failure,
    })
}

fn reset_once(
    ctx: &RpcSpawnContext,
    mut probe_options: ProbeOptions,
    selector: &DebugProbeSelector,
    method: ResetMethod,
    core: usize,
) -> RpcResult<Result<(), OpenFailure>> {
    if method == ResetMethod::Pin {
        // The pin reset itself drives the reset line.
        probe_options.connect_under_reset = false;
    }

    let lister = ctx.lister();
    let mut registry = ctx.registry_blocking();
    let loaded = probe_options.load(&mut registry)?;
    let mut probe = match open_probe(&lister, &loaded, selector) {
        Ok(probe) => probe,
        Err(failure) => return Ok(Err(failure)),
    };

    match (method, loaded.get_target_selector()?) {
        // Attaching under reset runs the reset sequences of the chip. Only targets behind an
        // ARM debug port have them.
        (ResetMethod::Pin, TargetSelector::Specified(target))
            if target.uses_arm_debug_interface() =>
        {
            let target = TargetSelector::Specified(target);
            let mut session = loaded.attach_session_with(probe, target, true)?;
            lift(session.resume_all_cores())?;
        }
        (ResetMethod::Pin, _) => {
            pulse_reset(&mut probe).map_err(|error| format!("{error:?}"))?;
            // A pin reset keeps the debug state of the cores, for example a halt that an
            // earlier session left.
            if let Err(error) = clear_debug_state(&loaded, probe) {
                tracing::warn!("Could not clear the debug state after the reset: {error:?}");
            }
        }
        (ResetMethod::Core, target) => {
            let mut session = loaded.attach_session(probe, target)?;
            let mut core = lift(session.core(core))?;
            lift(core.reset())?;
        }
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
    // Starting the wire protocol can fail on a target that does not answer, so try without it.
    let attached = match probe.target_reset_assert() {
        Ok(()) => false,
        Err(error) => {
            // Some probes drive the reset line only after they start the wire protocol.
            tracing::debug!("Asserting reset without the wire protocol failed: {error:?}");
            probe
                .attach_to_unspecified()
                .context("The probe cannot start the wire protocol")?;
            probe
                .target_reset_assert()
                .context("The probe cannot pulse the reset line")?;
            true
        }
    };
    std::thread::sleep(RESET_HOLD_TIME);
    probe
        .target_reset_deassert()
        .context("The probe cannot release the reset line. The target can stay in reset")?;
    if attached && let Err(error) = probe.detach() {
        tracing::warn!("Could not detach from the probe after the reset: {error:?}");
    }
    Ok(())
}

/// Selects the reset method. `method` overrides the probe settings, but a line that the settings
/// call unconnected cannot be pulsed.
fn reset_method(
    method: Option<ResetMethod>,
    reset_line_connected: Option<bool>,
) -> Result<ResetMethod, &'static str> {
    match (method, reset_line_connected) {
        (Some(ResetMethod::Pin), Some(false)) => {
            Err("`reset_line_connected` is false in the probe settings")
        }
        (Some(method), _) => Ok(method),
        (None, Some(true)) => Ok(ResetMethod::Pin),
        (None, _) => Ok(ResetMethod::Core),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_decide_without_a_method() {
        assert_eq!(reset_method(None, None), Ok(ResetMethod::Core));
        assert_eq!(reset_method(None, Some(false)), Ok(ResetMethod::Core));
        assert_eq!(reset_method(None, Some(true)), Ok(ResetMethod::Pin));
    }

    #[test]
    fn the_method_overrides_the_settings() {
        for connected in [None, Some(true)] {
            assert_eq!(
                reset_method(Some(ResetMethod::Pin), connected),
                Ok(ResetMethod::Pin)
            );
        }
        for connected in [None, Some(false), Some(true)] {
            assert_eq!(
                reset_method(Some(ResetMethod::Core), connected),
                Ok(ResetMethod::Core)
            );
        }
    }

    #[test]
    fn pin_is_refused_for_an_unconnected_line() {
        assert!(reset_method(Some(ResetMethod::Pin), Some(false)).is_err());
    }
}
