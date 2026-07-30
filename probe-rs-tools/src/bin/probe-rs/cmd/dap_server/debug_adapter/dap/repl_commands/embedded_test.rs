use crate::cmd::{
    dap_server::{
        DebuggerError,
        backend::rpc::RpcBackend,
        debug_adapter::{
            dap::{
                adapter::DebugAdapter,
                dap_types::EvaluateArguments,
                repl_commands::{EvalResponse, EvalResult, ReplCommand, async_fn, need_subcommand},
                repl_types::ReplCommandArgs,
            },
            protocol::ProtocolAdapter,
        },
        server::core_data::CoreData,
    },
    run::EmbeddedTestElfInfo,
};

pub(crate) static EMBEDDED_TEST: ReplCommand = ReplCommand {
    command: "test",
    help_text: "Interact with embedded-test test cases",
    requires_target_halted: false,
    sub_commands: &[
        ReplCommand {
            command: "list",
            help_text: "List all test cases.",
            requires_target_halted: false,
            sub_commands: &[],
            args: &[],
            handler: async_fn!(list_tests),
        },
        ReplCommand {
            command: "run",
            help_text: "Starts running a test case.",
            requires_target_halted: false,
            sub_commands: &[],
            args: &[ReplCommandArgs::Required("test_name")],
            handler: async_fn!(run_test),
        },
    ],
    args: &[],
    handler: async_fn!(need_subcommand),
};

async fn list_tests<'a>(
    _backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    _adapter: &'a mut DebugAdapter<dyn ProtocolAdapter + 'a>,
) -> EvalResult {
    let Some(test_data) = core_data.test_data.downcast_ref::<EmbeddedTestElfInfo>() else {
        return Err(DebuggerError::UserMessage(
            "Internal error while trying to access test data".to_string(),
        ));
    };
    let mut tests = test_data
        .tests
        .iter()
        .map(|t| t.name.as_str())
        .collect::<Vec<&str>>();
    tests.sort();
    Ok(EvalResponse::Message(tests.join("\n")))
}

async fn run_test<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    test_name: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    adapter: &'a mut DebugAdapter<dyn ProtocolAdapter + 'a>,
) -> EvalResult {
    let Some(test_data) = core_data.test_data.downcast_ref::<EmbeddedTestElfInfo>() else {
        return Err(DebuggerError::UserMessage(
            "Internal error while trying to access test data".to_string(),
        ));
    };

    let Some(test) = test_data.tests.iter().find(|test| test.name == test_name) else {
        return Err(DebuggerError::UserMessage(format!(
            "Test '{test_name}' not found"
        )));
    };

    let Some(address) = test.address else {
        return Err(DebuggerError::UserMessage(format!(
            "Test '{test_name}' has no address"
        )));
    };

    adapter
        .reset_and_halt_core_async(backend, core_data)
        .await?;
    backend
        .kickoff_test(core_data.core_index, address as u64)
        .await
        .map_err(DebuggerError::ProbeRs)?;

    core_data.last_known_status = probe_rs::CoreStatus::Unknown;
    adapter.all_cores_halted = false;

    Ok(EvalResponse::Message(String::new()))
}
