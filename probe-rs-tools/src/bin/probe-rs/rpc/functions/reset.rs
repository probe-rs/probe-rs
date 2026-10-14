use std::{thread::sleep, time::Duration};

use postcard_rpc::header::VarHeader;
use probe_rs_rpc::reset::{ResetCoreAndHaltRequest, ResetCoreRequest, ResetHardwareRequest};

use crate::rpc::functions::probe::convert::from_wire_debug_probe_selector;
use crate::rpc::functions::{RpcContext, convert::lift};
use crate::util::common_options::ProbeOptions;
use probe_rs_rpc::core_ops::WireCoreInformation;
use probe_rs_rpc::{NoResponse, RpcResult};

const RESET_HOLD_TIME: Duration = Duration::from_millis(100);
const RESET_POST_DELAY: Duration = Duration::from_millis(100);

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

/// Pulse the target's nRESET pin, without attaching to a target.
pub async fn reset_hardware(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ResetHardwareRequest,
) -> NoResponse {
    let mut registry = ctx.registry().await;
    let probe_options = ProbeOptions {
        probe: Some(from_wire_debug_probe_selector(request.probe.selector())),
        speed: request.speed,
        dry_run: request.dry_run,
        ..Default::default()
    }
    .load(&mut registry)?;

    let mut probe = probe_options.attach_probe(&ctx.lister())?;
    // Some probes will not drive the pin until they are connected to the target.
    lift(probe.attach_to_unspecified())?;

    lift(probe.target_reset_assert())?;
    sleep(RESET_HOLD_TIME);
    lift(probe.target_reset_deassert())?;
    sleep(RESET_POST_DELAY);

    Ok(())
}
