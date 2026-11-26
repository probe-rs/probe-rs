//! The test runner for embedded-test firmware.

use std::cell::RefCell;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use libtest_mimic::{Completion, Failed, Trial};
use tokio::runtime::Handle;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;

use crate::cmd::run::{EmbeddedTestElfInfo, MonitoringOptions};
use crate::rpc::Key;
use crate::rpc::RttClient;
use crate::util::style::strip_ansi;
use probe_rs_rpc::flash::BootInfo;
use probe_rs_rpc::test::{Test, TestResult};
use probe_rs_rpc_client::{ClientError, HostFiles, MonitorEvent, SessionInterface};

use super::{
    TargetOutput, display_stack_trace, format_stack_trace, parse_semihosting_options, send_cancel,
    with_ctrl_c,
};

pub async fn test(
    session: &SessionInterface,
    boot_info: BootInfo,
    elf_info: EmbeddedTestElfInfo,
    libtest_args: libtest_mimic::Arguments,
    monitor_options: &MonitoringOptions,
    path: &Path,
    mut output: TargetOutput,
) -> anyhow::Result<()> {
    tracing::info!("libtest args {:?}", libtest_args);
    let token = CancellationToken::new();

    let host_files = parse_semihosting_options(
        &monitor_options.semihosting_file,
        monitor_options.semihosting_allow_unverified_server,
    )?;

    let capture = !libtest_args.nocapture;

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<TestLogEvent>();

    let run = TestRun {
        session: session.clone(),
        path: path.to_path_buf(),
        rtt_client: output.rtt_handle(),
        host_files,
        sender,
        token: token.clone(),
        error: Arc::new(std::sync::Mutex::new(None)),
        stack_frame_limit: monitor_options.stack_frame_limit,
        capture,
        strip_colors: libtest_args.format == Some(libtest_mimic::FormatSetting::Json),
    };

    let test = async {
        let tests = if elf_info.version == 0 {
            // In embedded test < 0.7, we have to query the tests from the target via semihosting
            run.list_tests(boot_info).await?
        } else {
            // Recent embedded test versions report the tests directly via the elf file
            elf_info.tests
        };

        if token.is_cancelled() {
            return Ok(());
        }

        let error = run.error.clone();
        let tests = tests
            .into_iter()
            .map(|test| run.trial(test))
            .collect::<Vec<_>>();

        tokio::task::spawn_blocking(move || {
            let conclusion = libtest_mimic::run(&libtest_args, tests);
            if let Some(error) = error.lock().unwrap().take() {
                return Err(anyhow::Error::from(error));
            }
            if conclusion.has_failed() {
                anyhow::bail!("Some tests failed");
            }

            Ok(())
        })
        .await?
    };

    let log = async {
        log_test_output(&mut receiver, &mut output, capture).await;
        futures_util::future::pending().await
    };

    let test_and_log = async {
        tokio::select! {
            result = test => result,
            _ = log => anyhow::bail!("Log task resolved unexpectedly"),
        }
    };

    let result = with_ctrl_c(test_and_log, async {
        token.cancel();
        send_cancel(session).await;
    })
    .await;

    if token.is_cancelled() && monitor_options.always_print_stacktrace {
        display_stack_trace(session, path, monitor_options.stack_frame_limit).await?;
    }

    result
}

/// The state that the tests of one test run share.
#[derive(Clone)]
struct TestRun {
    session: SessionInterface,
    path: PathBuf,
    rtt_client: Option<Key<RttClient>>,
    host_files: Option<HostFiles>,
    sender: UnboundedSender<TestLogEvent>,
    token: CancellationToken,
    /// The first client error. The test command returns it after the test run.
    error: Arc<std::sync::Mutex<Option<ClientError>>>,
    stack_frame_limit: u32,
    capture: bool,
    strip_colors: bool,
}

impl TestRun {
    async fn list_tests(&self, boot_info: BootInfo) -> anyhow::Result<Vec<Test>> {
        self.sender.send(TestLogEvent::TestStart).unwrap();
        let result = self
            .session
            .list_tests(
                boot_info,
                self.rtt_client,
                self.host_files.clone(),
                async |msg| self.sender.send(TestLogEvent::Monitor(msg)).unwrap(),
            )
            .await;
        let output = self.end_test().await;

        // The output can tell why the list failed.
        match result {
            Ok(tests) => Ok(tests.tests),
            Err(e) => {
                print!("{output}");
                Err(e.into())
            }
        }
    }

    /// Ends the output of the current test. Returns the captured output.
    async fn end_test(&self) -> String {
        let (reply, captured) = tokio::sync::oneshot::channel();
        self.sender.send(TestLogEvent::TestEnd(reply)).unwrap();
        captured.await.unwrap_or_default()
    }

    fn trial(&self, test: Test) -> Trial {
        let name = test.name.clone();
        let ignored = test.ignored;
        let run = self.clone();

        Trial::ignorable_test(name, move || {
            // A cancel or an error ends the test run. The tests that did not run are ignored.
            if run.token.is_cancelled() || run.error.lock().unwrap().is_some() {
                return Ok(Completion::ignored_with("not run"));
            }

            let handle = tokio::spawn(async move { run.run_test(test).await });
            Handle::current().block_on(handle).unwrap()
        })
        .with_ignored_flag(ignored)
    }

    async fn run_test(self, test: Test) -> Result<Completion, Failed> {
        self.sender.send(TestLogEvent::TestStart).unwrap();
        let monitor_sender = self.sender.clone();
        let result = self
            .session
            .run_test(
                test,
                self.rtt_client,
                self.host_files.clone(),
                async move |msg| monitor_sender.send(TestLogEvent::Monitor(msg)).unwrap(),
            )
            .await;
        let output = self.end_test().await;

        let reason = match result {
            Ok(TestResult::Success) => return Ok(Completion::Completed),
            Ok(TestResult::Cancelled) => {
                print!("{output}");
                return Ok(Completion::ignored_with("cancelled"));
            }
            Ok(TestResult::Failed(message)) => message,
            Err(e) => {
                let message = e.to_string();
                *self.error.lock().unwrap() = Some(e);
                message
            }
        };

        let stack_trace =
            match format_stack_trace(&self.session, &self.path, self.stack_frame_limit).await {
                Ok(stack_trace) => stack_trace,
                Err(e) => format!("Failed to get the stack trace: {e}\n"),
            };

        if !self.capture {
            print!("{stack_trace}");
            return Err(Failed::from(reason));
        }

        Err(Failed::from(self.failure_report(
            output,
            &reason,
            &stack_trace,
        )))
    }

    /// Joins the output of a failed test, the failure and the stack trace.
    fn failure_report(&self, mut output: String, reason: &str, stack_trace: &str) -> String {
        // Same order as libtest: the output, then the failure.
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(reason);
        output.push('\n');
        output.push_str(stack_trace.trim_end());
        if self.strip_colors {
            output = strip_ansi(output);
        }
        output
    }
}

/// Writes the target output of a test run. In capture mode, collects the output of each test, and
/// drops the output outside of the tests.
async fn log_test_output(
    receiver: &mut UnboundedReceiver<TestLogEvent>,
    output: &mut TargetOutput,
    capture: bool,
) {
    let captured = RefCell::new(None::<CapturedOutput>);
    let write = |message: &str, print: fn(&str)| {
        if !capture {
            print(message);
        } else if let Some(captured) = captured.borrow_mut().as_mut() {
            captured.push(message);
        }
    };

    while let Some(event) = receiver.recv().await {
        match event {
            TestLogEvent::Monitor(event) => {
                output
                    .handle_event(
                        event,
                        &async |message| write(message, |m| print!("{m}")),
                        &async |message| write(message, |m| eprint!("{m}")),
                    )
                    .await;
            }
            TestLogEvent::TestStart if capture => {
                *captured.borrow_mut() = Some(CapturedOutput::default())
            }
            TestLogEvent::TestStart => {}
            TestLogEvent::TestEnd(reply) => {
                // The next test resets the target, so the decoders cannot complete this data.
                output
                    .flush(
                        &async |message| write(message, |m| print!("{m}")),
                        &async |message| write(message, |m| eprint!("{m}")),
                    )
                    .await;
                let text = captured.take().map(CapturedOutput::into_string);
                _ = reply.send(text.unwrap_or_default());
            }
        }
    }
}

/// A message to the log task of the test runner.
enum TestLogEvent {
    Monitor(MonitorEvent),
    TestStart,
    /// Sends the captured output of the test. The output is empty if capture is off.
    TestEnd(tokio::sync::oneshot::Sender<String>),
}

/// The output of one test. Keeps the last [`Self::LIMIT`] bytes.
#[derive(Default)]
struct CapturedOutput {
    text: String,
    truncated: bool,
}

impl CapturedOutput {
    const LIMIT: usize = 1024 * 1024;

    fn push(&mut self, message: &str) {
        self.text.push_str(message);
        // Trim at twice the limit, so that each push does not move the text.
        if self.text.len() > 2 * Self::LIMIT {
            self.trim();
        }
    }

    fn trim(&mut self) {
        if self.text.len() > Self::LIMIT {
            let start = self.text.ceil_char_boundary(self.text.len() - Self::LIMIT);
            self.text.drain(..start);
            self.truncated = true;
        }
    }

    fn into_string(mut self) -> String {
        self.trim();
        if self.truncated {
            format!("(The start of the output was removed.)\n{}", self.text)
        } else {
            self.text
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::cli::DecoderSettings;
    use probe_rs_rpc::monitor::SemihostingEvent;
    use std::collections::HashMap;

    /// Sends the semihosting output of each test through the log task. Returns the captured
    /// output of each test.
    async fn capture_tests(tests: &[&[u8]]) -> Vec<String> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut replies = Vec::new();
        for bytes in tests {
            sender.send(TestLogEvent::TestStart).unwrap();
            sender
                .send(TestLogEvent::Monitor(MonitorEvent::Semihosting(
                    SemihostingEvent::Output {
                        stream: "stdout".to_string(),
                        bytes: bytes.to_vec(),
                    },
                )))
                .unwrap();
            let (reply, captured) = tokio::sync::oneshot::channel();
            sender.send(TestLogEvent::TestEnd(reply)).unwrap();
            replies.push(captured);
        }
        drop(sender);

        let mut output =
            TargetOutput::from_parts(DecoderSettings::default(), None, HashMap::new(), Vec::new());
        log_test_output(&mut receiver, &mut output, true).await;

        let mut captured = Vec::new();
        for reply in replies {
            captured.push(reply.await.unwrap());
        }
        captured
    }

    #[tokio::test]
    async fn incomplete_output_stays_with_its_test() {
        // A trailing 0xFF can be the start of an esp-println defmt frame.
        let captured = capture_tests(&[b"output of the first test\n\xFF", b"second\n"]).await;
        assert_eq!(captured[1], "second\n");
    }
}
