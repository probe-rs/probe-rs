use crate::cmd::dap_server::{
    debug_adapter::{
        dap::{
            adapter::DebugAdapter,
            core_status::DapStatus,
            dap_types::EvaluateArguments,
            repl_commands::{EvalResponse, EvalResult, REPL_COMMANDS, ReplCommand},
        },
        protocol::ProtocolAdapter,
    },
    server::core_data::CoreHandle,
};

use linkme::distributed_slice;
use probe_rs::{CoreStatus, HaltReason};

#[distributed_slice(REPL_COMMANDS)]
static CONTINUE: ReplCommand = ReplCommand {
    command: "c",
    help_text: "Continue running the program on the target.",
    requires_target_halted: true,
    sub_commands: &[],
    args: &[],
    handler: r#continue,
};

#[distributed_slice(REPL_COMMANDS)]
static RESET: ReplCommand = ReplCommand {
    command: "reset",
    help_text: "Reset the target",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[],
    handler: reset,
};

#[distributed_slice(REPL_COMMANDS)]
static STEP: ReplCommand = ReplCommand {
    command: "step",
    help_text: "Step one instruction",
    requires_target_halted: true,
    sub_commands: &[],
    args: &[],
    handler: step,
};

fn r#continue(
    target_core: &mut CoreHandle<'_>,
    _: &str,
    _: &EvaluateArguments,
    _: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> EvalResult {
    target_core.core.run()?;

    Ok(EvalResponse::Message(
        CoreStatus::Running.short_long_status(None).1,
    ))
}

fn reset(
    target_core: &mut CoreHandle<'_>,
    _: &str,
    _: &EvaluateArguments,
    adapter: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> EvalResult {
    let core_info = target_core.reset_and_halt()?;
    adapter.pause_impl(target_core)?;

    Ok(EvalResponse::Message(
        CoreStatus::Halted(HaltReason::Request)
            .short_long_status(Some(core_info.pc))
            .1,
    ))
}

fn step(
    target_core: &mut CoreHandle<'_>,
    _: &str,
    _: &EvaluateArguments,
    _: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> EvalResult {
    let core_info = target_core.core.step()?;

    Ok(EvalResponse::Message(
        CoreStatus::Halted(HaltReason::Request)
            .short_long_status(Some(core_info.pc))
            .1,
    ))
}
