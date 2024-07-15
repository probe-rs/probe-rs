use crate::cmd::run::{print_stacktrace, OutputStream, ReturnReason, RunLoop, RunMode};
use anyhow::Result;
use libtest_mimic::{Arguments, Failed, FormatSetting, Trial};
use probe_rs::{BreakpointCause, Core, HaltReason, SemihostingCommand, Session};
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Options only used when in test run mode
#[derive(Debug, clap::Parser)]
pub struct TestOptions {
    /// Filter string. Only tests which contain this string are run.
    #[clap(
        index = 2,
        value_name = "TEST_FILTER",
        help = "The TEST_FILTER string is tested against the name of all tests, and only those tests whose names contain the filter are run. Multiple filter strings may be passed, which will run all tests matching any of the filters.",
        help_heading = "TEST OPTIONS"
    )]
    pub filter: Vec<String>,

    /// Only list all tests
    #[clap(
        long = "list",
        help = "List all tests instead of executing them",
        help_heading = "TEST OPTIONS"
    )]
    pub list: bool,

    #[clap(
        long = "format",
        value_enum,
        value_name = "pretty|terse|json",
        help_heading = "TEST OPTIONS",
        help = "Configure formatting of the test report output"
    )]
    pub format: Option<FormatSetting>,

    /// If set, filters are matched exactly rather than by substring.
    #[clap(long = "exact", help_heading = "TEST OPTIONS")]
    pub exact: bool,

    /// A list of filters. Tests whose names contain parts of any of these
    /// filters are skipped.
    #[clap(
        long = "skip-test",
        value_name = "FILTER",
        help_heading = "TEST OPTIONS",
        help = "Skip tests whose names contain FILTER (this flag can be used multiple times)"
    )]
    pub skip_test: Vec<String>,

    /// Options which are ignored, but exist for compatibility with libtest.
    /// E.g. so that vscode and intellij can invoke the test runner with the args they are used to
    #[clap(flatten)]
    _no_op: NoOpTestOptions,
}

/// Options which are ignored, but exist for compatibility with libtest.
#[derive(Debug, clap::Parser)]
struct NoOpTestOptions {
    // No-op, ignored (libtest-mimic always runs in no-capture mode)
    #[clap(long = "nocapture", hide = true)]
    nocapture: bool,

    /// No-op, ignored. libtest-mimic does not currently capture stdout.
    #[clap(long = "show-output", hide = true)]
    show_output: bool,

    /// No-op, ignored. Flag only exists for CLI compatibility with libtest.
    #[clap(short = 'Z', hide = true)]
    unstable_flags: Option<String>,
}

/// Test run mode
pub struct TestRunMode {
    libtest_args: Arguments,
}

impl TestRunMode {
    pub fn new(test_options: &TestOptions) -> Box<Self> {
        Box::new(Self {
            libtest_args: Arguments {
                test_threads: Some(1), // Avoid parallel execution
                list: test_options.list,
                exact: test_options.exact,
                format: test_options.format,
                skip: test_options.skip_test.clone(),
                filter: if test_options.filter.is_empty() {
                    None
                } else {
                    //TODO: Fix libtest-mimic so that it allows multiple filters (same as std test runners)
                    Some(test_options.filter.join(" "))
                },
                ..Arguments::default()
            },
        })
    }

    /// Asks the target for the tests, and create a "run the test"-closure for each test.
    /// libtest-mimic is in charge of selecting the tests to run based on the filter and other options
    fn create_tests(session_and_runloop_ref: Arc<Mutex<SessionAndRunLoop>>) -> Result<Vec<Trial>> {
        let mut session_and_runloop = session_and_runloop_ref.lock().unwrap();
        let list = Self::list_tests(&mut session_and_runloop)?;

        let mut tests = Vec::<Trial>::new();
        for t in &list.tests {
            let test = t.clone();
            let session_and_runloop = session_and_runloop_ref.clone();
            tests.push(
                Trial::test(&t.name, move || {
                    let mut session_and_runloop = session_and_runloop.lock().unwrap();
                    Self::run_test(test, &mut session_and_runloop)
                })
                .with_ignored_flag(t.ignored),
            )
        }
        Ok(tests)
    }

    const SEMIHOSTING_USER_LIST: u32 = 0x100;

    /// Requests all tests from the target via Semihosting back and forth
    fn list_tests(session_and_runloop: &mut SessionAndRunLoop) -> Result<Tests> {
        let mut core = session_and_runloop.session.core(0)?;

        let mut cmdline_requested = false;

        // When the target first invokes SYS_GET_CMDLINE (0x15), we answer "list"
        // Then, we wait until the target invokes SEMIHOSTING_USER_LIST (0x100) with the json containing all tests
        let halt_handler = |halt_reason: HaltReason, core: &mut Core| {
            let HaltReason::Breakpoint(BreakpointCause::Semihosting(cmd)) = halt_reason else {
                anyhow::bail!("CPU halted unexpectedly.");
            };

            match cmd {
                SemihostingCommand::GetCommandLine(request) if !cmdline_requested => {
                    tracing::debug!("target asked for cmdline. send 'list'");
                    cmdline_requested = true;
                    request.write_command_line_to_target(core, "list")?;
                    Ok(None) // Continue running
                }
                SemihostingCommand::Unknown(details)
                    if details.operation == Self::SEMIHOSTING_USER_LIST && cmdline_requested =>
                {
                    let buf = details.get_buffer(core)?;
                    let buf = buf.read(core)?;
                    let list = serde_json::from_slice::<Tests>(&buf[..])?;

                    // Signal status=success back to the target
                    details.write_status(core, 0)?;

                    tracing::debug!("got list of tests from target: {list:?}");
                    if list.version != 1 {
                        anyhow::bail!("Unsupported test list format version: {}", list.version);
                    }

                    Ok(Some(list))
                }
                other => anyhow::bail!(
                    "Unexpected semihosting command {:?} cmdline_requested: {:?}",
                    other,
                    cmdline_requested
                ),
            }
        };

        match session_and_runloop.run_loop.run_until(
            &mut core,
            true,
            true,
            OutputStream::Stderr,
            Some(Duration::from_secs(5)),
            halt_handler,
        )? {
            ReturnReason::User => anyhow::bail!(
                "The user pressed ctrl+c before the target responded with the test list."
            ),
            ReturnReason::Predicate(tests) => Ok(tests),
            ReturnReason::Timeout => {
                anyhow::bail!("The target did not respond with test list until timeout.")
            }
        }
    }

    /// Runs a single test on the target
    fn run_test(test: Test, session_and_runloop: &mut SessionAndRunLoop) -> Result<(), Failed> {
        let core = &mut session_and_runloop.session.core(0)?;
        tracing::info!("Running test {}", test.name);
        core.reset_and_halt(Duration::from_millis(100))?;

        let timeout = test.timeout.map(|t| Duration::from_secs(t as u64));
        let timeout = timeout.unwrap_or(Duration::from_secs(60)); // TODO: make global timeout configurable: https://github.com/probe-rs/embedded-test/issues/3
        let mut cmdline_requested = false;

        // When the target first invokes SYS_GET_CMDLINE (0x15), we answer "run <test_name>
        // Then we wait until the target invokes SYS_EXIT (0x18) or SYS_EXIT_EXTENDED(0x20) with the exit code
        let halt_handler = |halt_reason: HaltReason, core: &mut Core| {
            let cmd = match halt_reason {
                HaltReason::Breakpoint(BreakpointCause::Semihosting(cmd)) => cmd,
                e => {
                    // Exception occurred (e.g. hardfault) => Abort testing altogether
                    anyhow::bail!("The CPU halted unexpectedly: {:?}. Test should signal failure via a panic handler that calls `semihosting::proces::abort()` instead", e)
                }
            };

            match cmd {
                SemihostingCommand::GetCommandLine(request) if !cmdline_requested => {
                    let cmdline = format!("run {}", test.name);
                    tracing::debug!("target asked for cmdline. send '{cmdline}'");
                    cmdline_requested = true;
                    request.write_command_line_to_target(core, &cmdline)?;
                    Ok(None) // Continue running
                }
                SemihostingCommand::ExitSuccess if cmdline_requested => Ok(Some(TestOutcome::Pass)),

                SemihostingCommand::ExitError(_) if cmdline_requested => {
                    Ok(Some(TestOutcome::Panic))
                }

                other => {
                    // Invalid sequence of semihosting calls => Abort testing altogether
                    anyhow::bail!(
                        "Unexpected semihosting command {:?} cmdline_requested: {:?}",
                        other,
                        cmdline_requested
                    );
                }
            }
        };

        match session_and_runloop.run_loop.run_until(
            core,
            true,
            true,
            OutputStream::Stderr,
            Some(timeout),
            halt_handler,
        ) {
            Ok(ReturnReason::Timeout) => {
                Err(Failed::from(format!("Test timed out after {:?}", timeout)))
            }
            Ok(ReturnReason::User) => {
                eprintln!("Test {} was aborted by the user with CTRL + C", test.name);
                // We do not mark the test as failed and instead exit the process
                std::process::exit(1);
            }
            Ok(ReturnReason::Predicate(outcome)) => {
                if outcome == test.expected_outcome {
                    return Ok(());
                }

                if outcome == TestOutcome::Panic {
                    print_stacktrace(
                        core,
                        &session_and_runloop.run_loop.path,
                        &mut std::io::stderr(),
                    )?;
                }

                Err(Failed::from(format!(
                    "Test should {:?} but it did {:?}",
                    test.expected_outcome, outcome
                )))
            }
            Err(e) => {
                // Probe-rs error: We do not mark the test as failed and instead exit the process
                eprintln!("Error: {:?}", e);
                std::process::exit(1);
            }
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct Tests {
    pub version: u32,
    pub tests: Vec<Test>,
}

#[derive(PartialEq, Debug, Clone, Copy)]
enum TestOutcome {
    Panic,
    Pass,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Test {
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

impl RunMode for TestRunMode {
    fn run(&self, session: Session, run_loop: RunLoop) -> Result<()> {
        tracing::info!("libtest args {:?}", self.libtest_args);

        // Unfortunately libtest-mimic wants test functions to live for 'static, so we need to use a mutex to share the session and runloop
        let session_and_runloop = Arc::new(Mutex::new(SessionAndRunLoop { session, run_loop }));

        let tests = Self::create_tests(session_and_runloop)?;
        if libtest_mimic::run(&self.libtest_args, tests).has_failed() {
            anyhow::bail!("Some tests failed");
        }

        Ok(())
    }
}

struct SessionAndRunLoop {
    session: Session,
    run_loop: RunLoop,
}
