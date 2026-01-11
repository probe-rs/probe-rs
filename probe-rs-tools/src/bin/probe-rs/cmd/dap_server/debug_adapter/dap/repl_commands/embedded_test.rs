use std::time::Duration;

use probe_rs::{BreakpointCause, CoreStatus, HaltReason, semihosting::SemihostingCommand};

use crate::cmd::{
    dap_server::{
        DebuggerError,
        debug_adapter::dap::{
            core_status::DapStatus, dap_types::Response, repl_commands::ReplCommand,
            repl_types::ReplCommandArgs,
        },
    },
    run::EmbeddedTestElfInfo,
};

pub(crate) static EMBEDDED_TEST: ReplCommand = ReplCommand {
    command: "test",
    help_text: "Interact with embedded-test test cases",
    sub_commands: &[
        ReplCommand {
            command: "list",
            help_text: "List all test cases.",
            sub_commands: &[],
            args: &[],
            handler: |target_core, _, _| {
                let Some(test_data) = target_core
                    .core_data
                    .test_data
                    .downcast_ref::<EmbeddedTestElfInfo>()
                else {
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

                Ok(Response {
                    command: "tests".to_string(),
                    success: true,
                    message: Some(tests.join("\n")),
                    type_: "response".to_string(),
                    request_seq: 0,
                    seq: 0,
                    body: None,
                })
            },
        },
        ReplCommand {
            command: "run",
            help_text: "Starts running a test case.",
            sub_commands: &[],
            args: &[ReplCommandArgs::Required("test_name")],
            handler: |target_core, test_name, _| {
                let Some(test_data) = target_core
                    .core_data
                    .test_data
                    .downcast_ref::<EmbeddedTestElfInfo>()
                else {
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

                target_core.reset_and_halt()?;
                target_core.core.run()?;
                target_core
                    .core
                    .wait_for_core_halted(Duration::from_secs(1))?;

                let CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(
                    SemihostingCommand::GetCommandLine(request),
                ))) = target_core.core.status()?
                else {
                    return Err(DebuggerError::UserMessage(
                        "Could not start test".to_string(),
                    ));
                };

                // Select and start the test
                request.write_command_line_to_target(
                    &mut target_core.core,
                    &format!("run_addr {}", address),
                )?;
                target_core.core.run()?;

                target_core.core_data.last_known_status = CoreStatus::Running;

                // TODO: wait for a bit (while polling RTT) for the test to either complete
                // or the target to halt again? That way we could print the _actual_ test result
                // based on the expectation.

                Ok(Response {
                    command: "continue".to_string(),
                    success: true,
                    message: Some(CoreStatus::Running.short_long_status(None).1),
                    type_: "response".to_string(),
                    request_seq: 0,
                    seq: 0,
                    body: None,
                })
            },
        },
    ],
    args: &[],
    handler: |_, _, _| {
        Err(DebuggerError::UserMessage("Please provide one of the required subcommands. See the `help` command for more information.".to_string()))
    },
};
