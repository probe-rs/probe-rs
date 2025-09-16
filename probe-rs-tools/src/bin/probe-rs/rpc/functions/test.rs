use std::time::Duration;

use anyhow::Context;
use postcard_rpc::{header::VarHeader, server::Sender};
use postcard_schema::Schema;
use probe_rs::{BreakpointCause, Core, HaltReason, Session, semihosting::SemihostingCommand};
use serde::{Deserialize, Serialize};

use crate::{
    rpc::{
        Key,
        functions::{
            ListTestsEndpoint, RpcResult, RpcSpawnContext, RunTestEndpoint, WireTxImpl,
            flash::BootInfo,
            monitor::{MonitorSender, RttPoller, SemihostingEvent},
        },
        utils::{
            run_loop::{ReturnReason, RunLoop},
            semihosting::{SemihostingFileManager, SemihostingOptions},
        },
    },
    util::rtt::client::RttClient,
};

#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct Tests {
    pub version: u32,
    pub tests: Vec<Test>,
}

impl From<TestDefinitions> for Tests {
    fn from(def: TestDefinitions) -> Self {
        Self {
            version: def.version,
            tests: def.tests.into_iter().map(Test::from).collect(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestDefinitions {
    pub version: u32,
    pub tests: Vec<TestDefinition>,
}

#[derive(PartialEq, Debug, Clone, Copy, Serialize, Deserialize, Schema)]
pub enum TestOutcome {
    Panic,
    Pass,
}

#[derive(Debug, Clone, Serialize, Deserialize, Schema, PartialEq)]
pub struct Test {
    pub name: String,
    pub expected_outcome: TestOutcome,
    pub ignored: bool,
    pub timeout: Option<u32>,
    pub address: Option<u32>,
}

impl From<TestDefinition> for Test {
    fn from(def: TestDefinition) -> Self {
        Self {
            name: def.name,
            expected_outcome: def.expected_outcome,
            ignored: def.ignored,
            timeout: def.timeout,
            address: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestDefinition {
    pub name: String,
    #[serde(
        rename = "should_panic",
        deserialize_with = "outcome_from_should_panic"
    )]
    pub expected_outcome: TestOutcome,
    pub ignored: bool,
    pub timeout: Option<u32>,
}

fn outcome_from_should_panic<'de, D>(deserializer: D) -> Result<TestOutcome, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let should_panic = bool::deserialize(deserializer)?;
    Ok(if should_panic {
        TestOutcome::Panic
    } else {
        TestOutcome::Pass
    })
}

#[derive(Serialize, Deserialize, Schema)]
pub enum TestResult {
    Success,
    Failed(String),
    Cancelled,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct ListTestsRequest {
    pub sessid: Key<Session>,
    pub boot_info: BootInfo,
    /// RTT client if used.
    pub rtt_client: Option<Key<RttClient>>,
    pub semihosting_options: SemihostingOptions,
}

pub type ListTestsResponse = RpcResult<Tests>;

pub async fn list_tests(
    mut ctx: RpcSpawnContext,
    header: VarHeader,
    request: ListTestsRequest,
    sender: Sender<WireTxImpl>,
) {
    let resp = ctx
        .run_blocking::<MonitorSender, _, _, _>(request, list_tests_impl)
        .await
        .map_err(Into::into);

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
    let mut session = ctx.session_blocking(request.sessid);
    let mut list_handler = ListEventHandler::new(request.semihosting_options, |event| {
        sender.send_semihosting_event(event).unwrap()
    });

    let mut rtt_client = request
        .rtt_client
        .map(|rtt_client| ctx.object_mut_blocking(rtt_client));

    let core_id = rtt_client.as_ref().map(|rtt| rtt.core_id()).unwrap_or(0);

    let mut run_loop = RunLoop {
        core_id,
        cancellation_token: ctx.cancellation_token(),
    };

    request.boot_info.prepare(&mut session, run_loop.core_id)?;

    let mut core = session.core(0)?;
    if let Some(rtt_client) = rtt_client.as_mut() {
        rtt_client.clear_control_block(&mut core)?;
    }

    let poller = rtt_client.as_deref_mut().map(|client| RttPoller {
        rtt_client: client,
        sender: |message| {
            sender
                .send_rtt_event(message)
                .context("Failed to send RTT event")
        },
    });

    match run_loop.run_until(
        &mut core,
        true,
        true,
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

#[derive(Serialize, Deserialize, Schema)]
pub struct RunTestRequest {
    pub sessid: Key<Session>,
    pub test: Test,
    /// RTT client if used.
    pub rtt_client: Option<Key<RttClient>>,
    pub semihosting_options: SemihostingOptions,
}

pub type RunTestResponse = RpcResult<TestResult>;

pub async fn run_test(
    mut ctx: RpcSpawnContext,
    header: VarHeader,
    request: RunTestRequest,
    sender: Sender<WireTxImpl>,
) {
    let resp = ctx
        .run_blocking::<MonitorSender, _, _, _>(request, run_test_impl)
        .await
        .map_err(Into::into);

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

    let mut session = ctx.session_blocking(request.sessid);

    let mut rtt_client = request
        .rtt_client
        .map(|rtt_client| ctx.object_mut_blocking(rtt_client));

    let core_id = rtt_client.as_ref().map(|rtt| rtt.core_id()).unwrap_or(0);
    let mut core = session.core(core_id)?;
    core.reset_and_halt(Duration::from_millis(100))?;

    if let Some(rtt_client) = rtt_client.as_mut() {
        rtt_client.clear_control_block(&mut core)?;
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

    let poller = rtt_client.as_deref_mut().map(|client| RttPoller {
        rtt_client: client,
        sender: |message| {
            sender
                .send_rtt_event(message)
                .context("Failed to send RTT event")
        },
    });

    match run_loop.run_until(
        &mut core,
        true,
        true,
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
                "The CPU halted unexpectedly: {reason:?}. Test should signal failure via a panic handler that calls `semihosting::proces::abort()` instead",
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
