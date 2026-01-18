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
    adapter: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> EvalResult {
    adapter.continue_impl(target_core)?;
    let status = target_core.core.status()?;
    let pc = if status != CoreStatus::Running {
        let pc = target_core
            .core
            .read_core_reg::<u64>(target_core.core.program_counter().id)?;
        Some(pc)
    } else {
        None
    };

    Ok(EvalResponse::Message(status.short_long_status(pc).1))
}

fn reset(
    target_core: &mut CoreHandle<'_>,
    _: &str,
    _: &EvaluateArguments,
    adapter: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> EvalResult {
    let core_info = target_core.reset_and_halt()?;
    target_core.reset_core_status(adapter);

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
    adapter: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> EvalResult {
    let pc = adapter.step_impl(probe_rs_debug::SteppingMode::StepInstruction, target_core)?;

    Ok(EvalResponse::Message(
        CoreStatus::Halted(HaltReason::Request)
            .short_long_status(Some(pc))
            .1,
    ))
}
