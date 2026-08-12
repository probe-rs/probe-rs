use std::time::Duration;

use anyhow::Context;
use postcard_rpc::{header::VarHeader, server::Sender};
use probe_rs::{BreakpointCause, Core, HaltReason, semihosting::SemihostingCommand};
use probe_rs_rpc::test::{
    ListTestsRequest, RunTestRequest, Test, TestDefinitions, TestKickoffRequest,
    TestKickoffResponse, TestOutcome, TestResult, Tests,
};

use crate::rpc::{
    functions::{
        RpcContext, RpcSpawnContext, WireTxImpl,
        convert::lift,
        monitor::{MonitorSender, RttPoller},
    },
    utils::{
        run_loop::{ReturnReason, RunLoop, VectorCatchConfig},
        semihosting::SemihostingFileManager,
    },
};
use probe_rs_rpc::monitor::SemihostingEvent;
use probe_rs_rpc::semihosting_options::SemihostingOptions;
use probe_rs_rpc::{ListTestsEndpoint, RunTestEndpoint};

pub async fn list_tests(
    mut ctx: RpcSpawnContext,
    header: VarHeader,
    request: ListTestsRequest,
    sender: Sender<WireTxImpl>,
) {
    let resp = ctx
        .run_blocking::<MonitorSender, _, _, _>(request, list_tests_impl)
        .await
        .map_err(crate::rpc::functions::convert::rpc_error_anyhow);

    sender
        .reply::<ListTestsEndpoint>(header.seq_no, &resp)
        .await
        .unwrap();
}

fn list_tests_impl(
    ctx: RpcSpawnContext,
    request: ListTestsRequest,
    sender: MonitorSender,
) -> anyhow::Result<Tests> {
    let shared_session = ctx.shared_session(request.sessid);
    let mut list_handler = ListEventHandler::new(request.semihosting_options, |event| {
        sender.send_semihosting_event(event).unwrap()
    });

    let core_id = request
        .rtt_client
        .map(|rtt_client| ctx.object_mut_blocking(rtt_client).core_id())
        .unwrap_or(0);

    let mut run_loop = RunLoop {
        core_id,
        cancellation_token: ctx.cancellation_token(),
    };

    {
        let mut session = shared_session.session_blocking();
        crate::rpc::functions::flash::prepare_boot_info(
            &request.boot_info,
            &mut session,
            run_loop.core_id,
        )?;
    }

    let poller = request.rtt_client.map(|client| RttPoller {
        rtt_client: shared_session.object_storage().cell(client),
        clear_control_block: true,
        sender: |message| {
            sender
                .send_rtt_event(message)
                .context("Failed to send RTT event")
        },
    });

    match run_loop.run_until(
        &shared_session,
        VectorCatchConfig {
            catch_hardfault: true,
            catch_reset: true,
            catch_svc: true,
            catch_hlt: true,
        },
        poller,
        Some(Duration::from_secs(5)),
        |halt_reason, core| list_handler.handle_halt(halt_reason, core),
    )? {
        ReturnReason::Predicate(tests) => Ok(tests),
        ReturnReason::Timeout => {
            anyhow::bail!("The target did not respond with test list until timeout.")
        }
        ReturnReason::Cancelled => Ok(Tests {
            version: 1,
            tests: vec![],
        }),
        ReturnReason::LockedUp => {
            anyhow::bail!("The target locked up while waiting for the test list.")
        }
    }
}

pub async fn run_test(
    mut ctx: RpcSpawnContext,
    header: VarHeader,
    request: RunTestRequest,
    sender: Sender<WireTxImpl>,
) {
    let resp = ctx
        .run_blocking::<MonitorSender, _, _, _>(request, run_test_impl)
        .await
        .map_err(crate::rpc::functions::convert::rpc_error_anyhow);

    sender
        .reply::<RunTestEndpoint>(header.seq_no, &resp)
        .await
        .unwrap();
}

fn run_test_impl(
    ctx: RpcSpawnContext,
    request: RunTestRequest,
    sender: MonitorSender,
) -> anyhow::Result<TestResult> {
    tracing::info!("Running test {}", request.test.name);

    let timeout = request.test.timeout.map(|t| Duration::from_secs(t as u64));
    let timeout = timeout.unwrap_or(Duration::from_secs(60));

    let shared_session = ctx.shared_session(request.sessid);

    let core_id = request
        .rtt_client
        .map(|rtt_client| ctx.object_mut_blocking(rtt_client).core_id())
        .unwrap_or(0);

    {
        let mut session = shared_session.session_blocking();
        let mut core = session.core(core_id)?;
        core.reset_and_halt(Duration::from_millis(500))?;
    }

    let expected_outcome = request.test.expected_outcome;
    let mut run_handler =
        RunEventHandler::new(request.test, request.semihosting_options, |event| {
            sender.send_semihosting_event(event).unwrap()
        });

    let mut run_loop = RunLoop {
        core_id,
        cancellation_token: ctx.cancellation_token(),
    };

    let poller = request.rtt_client.map(|client| RttPoller {
        rtt_client: shared_session.object_storage().cell(client),
        clear_control_block: true,
        sender: |message| {
            sender
                .send_rtt_event(message)
                .context("Failed to send RTT event")
        },
    });

    match run_loop.run_until(
        &shared_session,
        VectorCatchConfig {
            catch_hardfault: true,
            catch_reset: true,
            catch_svc: true,
            catch_hlt: true,
        },
        poller,
        Some(timeout),
        |halt_reason, core| run_handler.handle_halt(halt_reason, core),
    )? {
        ReturnReason::Timeout => Ok(TestResult::Failed(format!(
            "Test timed out after {timeout:?}"
        ))),
        ReturnReason::Predicate(outcome) if outcome == expected_outcome => Ok(TestResult::Success),
        ReturnReason::Predicate(outcome) => Ok(TestResult::Failed(format!(
            "Test should {expected_outcome:?} but it did {outcome:?}"
        ))),
        ReturnReason::Cancelled => Ok(TestResult::Cancelled),
        ReturnReason::LockedUp => {
            anyhow::bail!("The target locked up while running the test.")
        }
    }
}

// -- test kickoff (DAP REPL `test run`) --------------------------------------

/// Kick off a single embedded-test case from the DAP REPL: run the core until
/// it halts on the `GetCommandLine` semihosting call, write the test address
/// as the command line, then resume. The subsequent test run is driven by
/// the DAP poll loop.
pub async fn test_kickoff(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: TestKickoffRequest,
) -> TestKickoffResponse {
    use probe_rs::CoreStatus;

    let mut session = ctx.session(request.sessid).await;
    let mut core = lift(session.core(request.core as usize))?;

    lift(core.run())?;
    lift(core.wait_for_core_halted(Duration::from_secs(1)))?;

    let CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(
        SemihostingCommand::GetCommandLine(cmd),
    ))) = lift(core.status())?
    else {
        Err("Could not start test: target did not halt on GetCommandLine")?
    };

    lift(cmd.write_command_line_to_target(&mut core, &format!("run_addr {}", request.address)))?;
    lift(core.run())?;
    Ok(())
}

struct ListEventHandler<F: FnMut(SemihostingEvent)> {
    semihosting_file_manager: SemihostingFileManager,
    cmdline_requested: bool,
    sender: F,
}

impl<F: FnMut(SemihostingEvent)> ListEventHandler<F> {
    const SEMIHOSTING_USER_LIST: u32 = 0x100;

    fn new(semihosting_options: SemihostingOptions, sender: F) -> Self {
        Self {
            semihosting_file_manager: SemihostingFileManager::new(semihosting_options),
            cmdline_requested: false,
            sender,
        }
    }

    fn handle_halt(
        &mut self,
        halt_reason: HaltReason,
        core: &mut Core<'_>,
    ) -> anyhow::Result<Option<Tests>> {
        let HaltReason::Breakpoint(BreakpointCause::Semihosting(cmd)) = halt_reason else {
            anyhow::bail!("CPU halted unexpectedly. Halt reason: {halt_reason:?}");
        };

        // When the target first invokes SYS_GET_CMDLINE (0x15), we answer "list"
        // Then, we wait until the target invokes SEMIHOSTING_USER_LIST (0x100) with the json containing all tests
        match cmd {
            SemihostingCommand::ExitSuccess => {
                anyhow::bail!("Application exited instead of providing a test list")
            }
            SemihostingCommand::ExitError(details) => anyhow::bail!(
                "Application exited with error {details} instead of providing a test list",
            ),
            SemihostingCommand::GetCommandLine(request) if !self.cmdline_requested => {
                tracing::debug!("target asked for cmdline. send 'list'");
                self.cmdline_requested = true;
                request.write_command_line_to_target(core, "list")?;
                Ok(None) // Continue running
            }
            SemihostingCommand::Unknown(details)
                if details.operation == Self::SEMIHOSTING_USER_LIST && self.cmdline_requested =>
            {
                let list = read_test_list(details, core)?;

                tracing::debug!("got list of tests from target: {list:?}");
                if list.version != 1 {
                    anyhow::bail!("Unsupported test list format version: {}", list.version);
                }

                Ok(Some(list.into()))
            }
            other if SemihostingFileManager::can_handle(other) => {
                self.semihosting_file_manager
                    .handle(other, core, &mut self.sender)?;
                Ok(None)
            }
            SemihostingCommand::Time(request) => {
                request.write_current_time(core)?;
                Ok(None)
            }
            SemihostingCommand::Errno(_) => Ok(None),
            other => anyhow::bail!(
                "Unexpected semihosting command {:?} cmdline_requested: {:?}",
                other,
                self.cmdline_requested
            ),
        }
    }
}

fn read_test_list(
    details: probe_rs::semihosting::UnknownCommandDetails,
    core: &mut Core<'_>,
) -> anyhow::Result<TestDefinitions> {
    let buf = details.get_buffer(core)?;
    let buf = buf.read(core)?;
    let list = serde_json::from_slice::<TestDefinitions>(&buf[..])?;

    // Signal status=success back to the target
    details.write_status(core, 0)?;

    Ok(list)
}

struct RunEventHandler<F: FnMut(SemihostingEvent)> {
    semihosting_file_manager: SemihostingFileManager,
    cmdline_requested: bool,
    test: Test,
    sender: F,
}

impl<F: FnMut(SemihostingEvent)> RunEventHandler<F> {
    fn new(test: Test, semihosting_options: SemihostingOptions, sender: F) -> Self {
        Self {
            test,
            semihosting_file_manager: SemihostingFileManager::new(semihosting_options),
            cmdline_requested: false,
            sender,
        }
    }

    fn handle_halt(
        &mut self,
        halt_reason: HaltReason,
        core: &mut Core<'_>,
    ) -> anyhow::Result<Option<TestOutcome>> {
        let cmd = match halt_reason {
            HaltReason::Breakpoint(BreakpointCause::Semihosting(cmd)) => cmd,
            // Exception occurred (e.g. hardfault) => Abort testing altogether
            reason => anyhow::bail!(
                "The CPU halted unexpectedly: {reason:?}. Test should signal failure via a panic handler that calls `semihosting::process::abort()` instead",
            ),
        };

        match cmd {
            SemihostingCommand::GetCommandLine(request) if !self.cmdline_requested => {
                let cmdline = if let Some(address) = self.test.address {
                    format!("run_addr {address}")
                } else {
                    format!("run {}", self.test.name)
                };
                tracing::debug!("target asked for cmdline. send '{cmdline}'");
                self.cmdline_requested = true;
                request.write_command_line_to_target(core, &cmdline)?;
                Ok(None) // Continue running
            }
            SemihostingCommand::ExitSuccess if self.cmdline_requested => {
                Ok(Some(TestOutcome::Pass))
            }

            SemihostingCommand::ExitError(_) if self.cmdline_requested => {
                Ok(Some(TestOutcome::Panic))
            }
            other if SemihostingFileManager::can_handle(other) => {
                self.semihosting_file_manager
                    .handle(other, core, &mut self.sender)?;
                Ok(None)
            }
            SemihostingCommand::Time(request) => {
                request.write_current_time(core)?;
                Ok(None)
            }
            SemihostingCommand::Errno(_) => Ok(None),
            // Invalid sequence of semihosting calls => Abort testing altogether
            other => anyhow::bail!(
                "Unexpected semihosting command {:?} cmdline_requested: {:?}",
                other,
                self.cmdline_requested
            ),
        }
    }
}
