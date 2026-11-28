use crate::cmd::{
    dap_server::{
        DebuggerError,
        backend::rpc::RpcBackend,
        debug_adapter::dap::{
            adapter::{DebugAdapter, reapply_breakpoints_after_reset},
            dap_types::EvaluateArguments,
            repl_commands::{EvalResponse, EvalResult, ReplCommand, async_fn, need_subcommand},
            repl_types::ReplCommandArgs,
        },
        server::core_data::CoreData,
    },
    run::EmbeddedTestElfInfo,
};
use probe_rs::CoreStatus;
use probe_rs_rpc::test::Test;
use std::time::Duration;

pub(crate) static EMBEDDED_TEST: ReplCommand = ReplCommand {
    command: "test",
    help_text: "Interact with embedded-test test cases",
    requires_target_halted: false,
    sub_commands: &[
        ReplCommand {
            command: "list",
            help_text: "List all test cases. For embedded-test before 0.7, the first call resets the target to ask it for the list.",
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
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    adapter: &'a mut DebugAdapter,
) -> EvalResult {
    let mut tests = embedded_tests(backend, core_data, adapter)
        .await?
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
    adapter: &'a mut DebugAdapter,
) -> EvalResult {
    let Some(test) = embedded_tests(backend, core_data, adapter)
        .await?
        .iter()
        .find(|test| test.name == test_name)
        .cloned()
    else {
        return Err(DebuggerError::UserMessage(format!(
            "Test '{test_name}' not found"
        )));
    };

    adapter
        .reset_and_halt_core_async(backend, core_data)
        .await?;
    backend
        .kickoff_test(core_data.core_index, test)
        .await
        .map_err(DebuggerError::ProbeRs)?;

    core_data.last_known_status = CoreStatus::Unknown;
    adapter.all_cores_halted = false;

    Ok(EvalResponse::Message(String::new()))
}

/// The embedded-test data of a core.
pub(crate) struct EmbeddedTests {
    /// `None` until the first listing for protocol 0 binaries, which report their tests only at
    /// run time.
    tests: Option<Vec<Test>>,
}

impl From<EmbeddedTestElfInfo> for EmbeddedTests {
    fn from(info: EmbeddedTestElfInfo) -> Self {
        Self {
            tests: (info.version != 0).then_some(info.tests),
        }
    }
}

/// The tests of the binary. For a protocol 0 binary, the first call resets the target and runs it
/// until it reports its tests.
async fn embedded_tests<'a>(
    backend: &mut RpcBackend,
    core_data: &'a mut CoreData,
    adapter: &mut DebugAdapter,
) -> Result<&'a [Test], DebuggerError> {
    let internal_error = || {
        DebuggerError::UserMessage("Internal error while trying to access test data".to_string())
    };

    let listed = core_data
        .test_data
        .downcast_ref::<EmbeddedTests>()
        .ok_or_else(internal_error)?
        .tests
        .is_some();
    if !listed {
        // The listing RPC selects the core through an RTT client, and the DAP RTT client must keep
        // its data.
        if core_data.core_index != 0 {
            return Err(DebuggerError::UserMessage(
                "Listing the tests of a binary built with embedded-test before 0.7 works only on core 0."
                    .to_string(),
            ));
        }

        core_data.invalidate_stack_frame_cache();
        core_data.last_known_status = CoreStatus::Unknown;
        adapter.all_cores_halted = false;

        // The listing resets the target, also when it fails. A listing that times out leaves the
        // core running.
        let listing = backend.list_tests().await;
        if listing.is_err() {
            _ = backend
                .halt(core_data.core_index, Duration::from_millis(100))
                .await;
        }
        reapply_breakpoints_after_reset(backend, core_data).await?;
        let tests = listing.map_err(DebuggerError::ProbeRs)?;

        let test_data = core_data
            .test_data
            .downcast_mut::<EmbeddedTests>()
            .ok_or_else(internal_error)?;
        return Ok(test_data.tests.insert(tests));
    }

    core_data
        .test_data
        .downcast_ref::<EmbeddedTests>()
        .and_then(|test_data| test_data.tests.as_deref())
        .ok_or_else(internal_error)
}
