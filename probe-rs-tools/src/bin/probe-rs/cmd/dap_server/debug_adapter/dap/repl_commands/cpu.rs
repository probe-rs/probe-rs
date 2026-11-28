use crate::cmd::dap_server::{
    DebuggerError,
    backend::rpc::RpcBackend,
    debug_adapter::dap::{
        adapter::DebugAdapter,
        core_status::DapStatus,
        dap_types::EvaluateArguments,
        repl_commands::{EvalResponse, EvalResult, REPL_COMMANDS, ReplCommand, async_fn},
        repl_types::ReplCommandArgs,
    },
    server::core_data::CoreData,
};
use linkme::distributed_slice;
use probe_rs::{CoreStatus, HaltReason};
use probe_rs_debug::SteppingMode;
use std::time::Duration;

#[distributed_slice(REPL_COMMANDS)]
static CONTINUE: ReplCommand = ReplCommand {
    command: "c",
    help_text: "Continue running the program on the target.",
    requires_target_halted: true,
    sub_commands: &[],
    args: &[],
    handler: async_fn!(continue_repl),
};

#[distributed_slice(REPL_COMMANDS)]
static WAIT: ReplCommand = ReplCommand {
    command: "wait",
    help_text: "Wait until the core halts. The next command ends the wait.",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[ReplCommandArgs::Optional("timeout in seconds")],
    handler: async_fn!(wait_repl),
};

#[distributed_slice(REPL_COMMANDS)]
static RESET: ReplCommand = ReplCommand {
    command: "reset",
    help_text: "Reset the target",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[],
    handler: async_fn!(reset_repl),
};

#[distributed_slice(REPL_COMMANDS)]
static STEP: ReplCommand = ReplCommand {
    command: "step",
    help_text: "Step one instruction.",
    requires_target_halted: true,
    sub_commands: &[
        ReplCommand {
            command: "over",
            help_text: "Step over the current statement.",
            requires_target_halted: true,
            sub_commands: &[],
            args: &[],
            handler: async_fn!(step_over_repl),
        },
        ReplCommand {
            command: "into",
            help_text: "Step into the current statement.",
            requires_target_halted: true,
            sub_commands: &[],
            args: &[],
            handler: async_fn!(step_into_repl),
        },
        ReplCommand {
            command: "out",
            help_text: "Step out of the current function.",
            requires_target_halted: true,
            sub_commands: &[],
            args: &[],
            handler: async_fn!(step_out_repl),
        },
    ],
    args: &[],
    handler: async_fn!(step_repl),
};

async fn continue_repl<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    adapter: &'a mut DebugAdapter,
) -> EvalResult {
    adapter.continue_impl_async(backend, core_data).await?;
    Ok(EvalResponse::Message(String::new()))
}

async fn wait_repl<'a>(
    _backend: &'a mut RpcBackend,
    _core_data: &'a mut CoreData,
    command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    _adapter: &'a mut DebugAdapter,
) -> EvalResult {
    Ok(EvalResponse::Wait(parse_wait_timeout(command_arguments)?))
}

fn parse_wait_timeout(argument: &str) -> Result<Option<Duration>, DebuggerError> {
    match argument.trim() {
        "" => Ok(None),
        seconds => seconds
            .parse()
            .map(|seconds| Some(Duration::from_secs(seconds)))
            .map_err(|_| {
                DebuggerError::UserMessage(format!(
                    "Invalid timeout {seconds:?}: give a whole number of seconds."
                ))
            }),
    }
}

async fn reset_repl<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    adapter: &'a mut DebugAdapter,
) -> EvalResult {
    adapter
        .reset_and_halt_core_async(backend, core_data)
        .await?;
    Ok(EvalResponse::Message(String::new()))
}

async fn step_repl<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    adapter: &'a mut DebugAdapter,
) -> EvalResult {
    step_with_mode(backend, core_data, adapter, SteppingMode::StepInstruction).await
}

async fn step_over_repl<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    adapter: &'a mut DebugAdapter,
) -> EvalResult {
    step_with_mode(backend, core_data, adapter, SteppingMode::OverStatement).await
}

async fn step_into_repl<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    adapter: &'a mut DebugAdapter,
) -> EvalResult {
    step_with_mode(backend, core_data, adapter, SteppingMode::IntoStatement).await
}

async fn step_out_repl<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    adapter: &'a mut DebugAdapter,
) -> EvalResult {
    step_with_mode(backend, core_data, adapter, SteppingMode::OutOfStatement).await
}

async fn step_with_mode(
    backend: &mut RpcBackend,
    core_data: &mut CoreData,
    adapter: &mut DebugAdapter,
    mode: SteppingMode,
) -> EvalResult {
    let pc = adapter.step_impl_async(mode, backend, core_data).await?;
    Ok(EvalResponse::Message(
        CoreStatus::Halted(HaltReason::Step)
            .short_long_status(backend, Some(pc), adapter.supports_ansi_styling)
            .await
            .1,
    ))
}

#[cfg(test)]
mod test {
    use super::parse_wait_timeout;
    use std::time::Duration;

    #[test]
    fn wait_timeout_is_whole_seconds_or_none() {
        assert_eq!(parse_wait_timeout(" ").unwrap(), None);
        assert_eq!(
            parse_wait_timeout("5").unwrap(),
            Some(Duration::from_secs(5))
        );
        assert!(parse_wait_timeout("5s").is_err());
        assert!(parse_wait_timeout("0.5").is_err());
    }
}
