use crate::cmd::dap_server::{
    backend::rpc::RpcBackend,
    debug_adapter::{
        dap::{
            adapter::DebugAdapter,
            core_status::DapStatus,
            dap_types::EvaluateArguments,
            repl_commands::{EvalResponse, EvalResult, REPL_COMMANDS, ReplCommand, async_fn},
        },
        protocol::ProtocolAdapter,
    },
    server::core_data::CoreData,
};
use linkme::distributed_slice;
use probe_rs::{CoreStatus, HaltReason};
use probe_rs_debug::SteppingMode;

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
    help_text: "Step one instruction",
    requires_target_halted: true,
    sub_commands: &[],
    args: &[],
    handler: async_fn!(step_repl),
};

async fn continue_repl<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    adapter: &'a mut DebugAdapter<dyn ProtocolAdapter + 'a>,
) -> EvalResult {
    adapter.continue_impl_async(backend, core_data).await?;
    Ok(EvalResponse::Message(String::new()))
}

async fn reset_repl<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    adapter: &'a mut DebugAdapter<dyn ProtocolAdapter + 'a>,
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
    adapter: &'a mut DebugAdapter<dyn ProtocolAdapter + 'a>,
) -> EvalResult {
    let pc = adapter
        .step_impl_async(SteppingMode::StepInstruction, backend, core_data)
        .await?;
    Ok(EvalResponse::Message(
        CoreStatus::Halted(HaltReason::Request)
            .short_long_status(Some(pc))
            .1,
    ))
}
