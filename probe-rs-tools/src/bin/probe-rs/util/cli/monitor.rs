//! The monitor: shows the target output and sends user input to the target.

use std::future::pending;
use std::io::{IsTerminal, Write};
use std::ops::ControlFlow;
use std::time::Duration;
use std::{future::Future, path::Path, time::Instant};

use rustyline_async::{Readline, ReadlineError, ReadlineEvent, SharedWriter};
use tokio::sync::futures::Notified;
use tokio::sync::{Mutex, MutexGuard, Notify};

use crate::cmd::run::MonitoringOptions;
use crate::rpc::Key;
use crate::rpc::RttClient;
use crate::rpc::utils::run_loop::VectorCatchConfig;
use crate::util::{logging, style::Prompt};
use probe_rs_rpc::core_ops::{WireBreakpointCause, WireHaltReason};
use probe_rs_rpc::monitor::{ChannelInfo, MonitorExitReason};
use probe_rs_rpc::monitor::{MonitorMode, MonitorOptions, RttEvent, SemihostingExitError};
use probe_rs_rpc_client::{ClientError, MonitorEvent, SessionInterface};

use super::{
    TargetOutput, display_stack_trace, parse_semihosting_options, send_cancel, terminate_signal,
};

// Monitor starts in read-only mode: it outputs logs, but has no prompt to type into.
// When channels are discovered, it can either stay in read-only mode, or switch to interactive mode if down channels are available.
// Interactive mode allows the user to type into the prompt, and send data to the target.

struct MonitorUiContext {
    change_notifier: Notify,
    ui_state: Mutex<MonitorUiState>,
}

impl MonitorUiContext {
    pub fn new(selected_down_channel: u32) -> Self {
        let change_notifier = Notify::new();
        let ui_state = Mutex::new(MonitorUiState {
            exited: false,
            rtt_client: None,
            up_channels: Vec::new(),
            down_channels: Vec::new(),
            selected_down_channel,
            serial_input: false,
            shared_writer: None,
        });
        Self {
            change_notifier,
            ui_state,
        }
    }

    async fn exit(&self) {
        self.ui_state.lock().await.exit();
        self.change_notifier.notify_waiters();
    }

    async fn update(&self, with: impl FnOnce(&mut MonitorUiState)) {
        let mut ui_state = self.ui_state.lock().await;
        with(&mut ui_state);
        self.change_notifier.notify_waiters();
    }

    /// Updates the state from a monitor event.
    async fn observe(&self, event: &MonitorEvent, rtt_client: Option<Key<RttClient>>) {
        match event {
            MonitorEvent::Rtt(RttEvent::Discovered {
                down_channels,
                up_channels,
            }) => {
                self.update(|state| {
                    state.up_channels = up_channels.clone();
                    state.down_channels = down_channels.clone();
                    state.rtt_client = rtt_client;
                })
                .await
            }
            MonitorEvent::Serial(_) if !self.lock().await.serial_input => {
                self.update(|state| state.serial_input = true).await
            }
            _ => {}
        }
    }

    fn subscribe(&self) -> Notified<'_> {
        self.change_notifier.notified()
    }

    fn lock(&self) -> impl Future<Output = MutexGuard<'_, MonitorUiState>> {
        self.ui_state.lock()
    }
}

#[derive(Clone)]
struct MonitorUiState {
    exited: bool,
    rtt_client: Option<Key<RttClient>>,
    up_channels: Vec<ChannelInfo>,
    down_channels: Vec<ChannelInfo>,
    selected_down_channel: u32,
    /// The target serial port sent data, so it can receive input too.
    serial_input: bool,
    shared_writer: Option<SharedWriter>,
}
/// A target of the user input.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InputTarget {
    Rtt(u32),
    Serial,
}

impl MonitorUiState {
    /// The input targets: the RTT down channels, then the serial port.
    fn input_targets(&self) -> Vec<(InputTarget, String)> {
        let mut targets = self
            .down_channels
            .iter()
            .enumerate()
            .map(|(i, channel)| (InputTarget::Rtt(i as u32), channel.name.clone()))
            .collect::<Vec<_>>();
        if self.serial_input {
            targets.push((InputTarget::Serial, "serial".to_string()));
        }
        targets
    }

    fn print(&mut self, message: &str) {
        if let Some(writer) = self.shared_writer.as_mut() {
            _ = writer.write_all(message.as_bytes());
        } else {
            print!("{message}");
        }
    }

    fn exit(&mut self) {
        self.exited = true;
        self.shared_writer = None;
    }

    fn display_mode(&self, list_rtt: bool) -> DisplayMode {
        if self.exited {
            DisplayMode::Exited
        } else if list_rtt {
            DisplayMode::ListChannelsAndQuit
        } else if self.input_targets().is_empty() || !prompt_possible() {
            DisplayMode::OutputOnly
        } else {
            DisplayMode::CliWithPrompt
        }
    }
}

pub async fn monitor(
    session: &SessionInterface,
    mode: MonitorMode,
    path: Option<&Path>,
    monitor_options: &MonitoringOptions,
    mut output: TargetOutput,
    vector_catch: VectorCatchConfig,
) -> anyhow::Result<()> {
    let semihosting_options = parse_semihosting_options(&monitor_options.semihosting_file)?;
    output.route(monitor_options).await?;

    let options = MonitorOptions {
        catch_reset: vector_catch.catch_reset,
        catch_hardfault: vector_catch.catch_hardfault,
        catch_svc: vector_catch.catch_svc,
        catch_hlt: vector_catch.catch_hlt,
        rtt_client: output.rtt_handle(),
        semihosting_options,
    };

    // The mutex around the context should only be held for a short period of time.
    let ui_context = MonitorUiContext::new(monitor_options.rtt_down_channel);

    let monitor = session.monitor(mode, options, async |msg| {
        ui_context.observe(&msg, output.rtt_handle()).await;
        output
            .handle_event(
                msg,
                &async |message| ui_context.lock().await.print(message),
                &async |message| eprint!("{message}"),
            )
            .await;
    });

    // SIGTERM (Unix) / Ctrl+Break (Windows) handler
    let terminate = async {
        let signal = terminate_signal().await;
        eprintln!("Received {signal}, exiting");
        send_cancel(session).await;
        pending().await
    };

    let ui = async {
        run_monitor_ui(session, &ui_context, monitor_options.list_rtt).await;
        send_cancel(session).await;
        pending().await
    };

    // We exit when one of the futures cancels the session and the monitor exits.
    // TODO: this should be a loop
    let result = tokio::select! {
        result = monitor => result,

        // These futures are never supposed to resolve. They shall trigger
        // a cancellation event, then the monitor future will handle the rest.
        _ = ui => unreachable!(),
        _ = terminate => unreachable!(),
    };

    let (print_stack_trace, result) =
        monitor_outcome(result, monitor_options.always_print_stacktrace);

    if print_stack_trace {
        try_display_stack_trace(session, path, monitor_options.stack_frame_limit).await;
    }

    println!("{}", result?);
    Ok(())
}

/// What the monitor UI shows.
enum DisplayMode {
    OutputOnly,
    CliWithPrompt,
    ListChannelsAndQuit,
    Exited,
}

fn prompt_possible() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

// Main UI loop. Detects changes generated either by the user or received from
// the server, and decides what to display based on the current state.
async fn run_monitor_ui(session: &SessionInterface, context: &MonitorUiContext, list_rtt: bool) {
    const LIST_RTT_TIMEOUT: Duration = Duration::from_secs(5);
    let list_rtt_deadline = Instant::now() + LIST_RTT_TIMEOUT;

    loop {
        // Release the lock before the mode runs.
        let mode = context.lock().await.display_mode(list_rtt);
        match mode {
            DisplayMode::OutputOnly => wait_for_change(context).await,
            DisplayMode::CliWithPrompt => cli_with_prompt(session, context).await,
            DisplayMode::ListChannelsAndQuit => list_channels(context, list_rtt_deadline).await,
            DisplayMode::Exited => break,
        }
    }
}

async fn wait_for_change(context: &MonitorUiContext) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("Received Ctrl+C, exiting");
            context.exit().await;
        },
        _ = context.subscribe() => {}
    }
}

/// Prints the RTT channels, then exits. Waits for the RTT client until `deadline`.
async fn list_channels(context: &MonitorUiContext, deadline: Instant) {
    // Subscribe before the state check so that a discovery
    // notification is not lost between the check and the wait.
    let notified = context.subscribe();
    let mut data = context.lock().await;
    if data.rtt_client.is_some() {
        println!("Up channels:");
        print_channels(&data.up_channels);
        println!("Down channels:");
        print_channels(&data.down_channels);
        data.exit();
        return;
    }

    drop(data);
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("Received Ctrl+C, exiting");
            context.exit().await;
        }
        _ = tokio::time::sleep(deadline.saturating_duration_since(Instant::now())) => {
            eprintln!("Failed to attach to RTT: Timeout");
            context.exit().await;
        }
        _ = notified => {}
    }
}

fn print_channels(channels: &[ChannelInfo]) {
    if channels.is_empty() {
        println!("  None.");
        return;
    }
    for (i, channel) in channels.iter().enumerate() {
        println!("  {}: {}", i, ChannelInfoPrinter(channel));
    }
}

fn prompt_text(targets: &[(InputTarget, String)], selected: InputTarget) -> String {
    let name = targets
        .iter()
        .find(|(target, _)| *target == selected)
        .map_or("", |(_, name)| name.as_str());
    Prompt::new(format!("{name}> ")).to_string()
}

/// Returns the input target after `selected`. Wraps around at the end of `targets`.
fn next_input_target(targets: &[(InputTarget, String)], selected: InputTarget) -> InputTarget {
    let next = targets
        .iter()
        .position(|(target, _)| *target == selected)
        .map_or(0, |i| (i + 1) % targets.len());
    targets[next].0
}

// Gets activated when the RTT client discovers down channels, or the target serial port
// sends data. Displays a prompt and waits for user input. The input targets can change while
// the prompt runs, so the prompt reads them again for each event.
async fn cli_with_prompt(session: &SessionInterface, context: &MonitorUiContext) {
    let (targets, selected_down_channel) = {
        let state = context.lock().await;
        (state.input_targets(), state.selected_down_channel)
    };
    let mut selected = targets
        .iter()
        .map(|(target, _)| *target)
        .find(|target| *target == InputTarget::Rtt(selected_down_channel))
        .unwrap_or(targets[0].0);

    let Ok((mut rl, sw)) = Readline::new(prompt_text(&targets, selected)) else {
        eprintln!("Failed to create readline");
        _ = tokio::signal::ctrl_c().await;

        eprintln!("Received Ctrl+C, exiting");
        return;
    };

    let _prompt_logs = logging::install_prompt_writer(sw.clone());

    context
        .update(|data| data.shared_writer = Some(sw.clone()))
        .await;

    rl.should_print_line_on(true, false);
    loop {
        match rl.readline().await {
            Ok(ReadlineEvent::Line(mut line)) => {
                rl.add_history_entry(line.clone());
                line.push('\n');
                if send_line(session, context, selected, line).await.is_break() {
                    break;
                }
            }
            Ok(ReadlineEvent::Eof) => {
                let targets = context.lock().await.input_targets();
                selected = next_input_target(&targets, selected);
                if let Err(error) = rl.update_prompt(&prompt_text(&targets, selected)) {
                    eprintln!("Error updating prompt: {:?}", error);
                    break;
                }
            }
            Ok(ReadlineEvent::Interrupted) => {
                eprintln!("Received Ctrl+C, exiting");
                break;
            }
            Err(ReadlineError::Closed) => break,
            Err(ReadlineError::IO(err)) => {
                eprintln!("IO error: {}", err);
                break;
            }
        }
    }

    context.exit().await;

    _ = rl.flush();
}

/// Sends a line of user input to `target`. Breaks if the prompt must stop.
async fn send_line(
    session: &SessionInterface,
    context: &MonitorUiContext,
    target: InputTarget,
    line: String,
) -> ControlFlow<()> {
    match target {
        InputTarget::Rtt(channel) => {
            let Some(client) = context.lock().await.rtt_client else {
                return ControlFlow::Continue(());
            };
            if let Err(error) = session
                .send_to_rtt(client, channel, line.into_bytes(), 0)
                .await
            {
                eprintln!("Error sending data to RTT: {:?}", error);
                return ControlFlow::Break(());
            }
        }
        InputTarget::Serial => {
            let len = line.len();
            // The firmware may not read its input. Report it and continue.
            match session.send_to_serial(line.into_bytes()).await {
                Ok(written) if written as usize == len => {}
                Ok(written) => eprintln!("The serial port took {written} of {len} bytes"),
                Err(error) => eprintln!("Error sending data to the serial port: {error:?}"),
            }
        }
    }
    ControlFlow::Continue(())
}

/// Returns whether to print a stack trace, and the message to print when the firmware ran
/// successfully.
fn monitor_outcome(
    result: Result<MonitorExitReason, ClientError>,
    always_print_stacktrace: bool,
) -> (bool, anyhow::Result<&'static str>) {
    match result {
        Ok(MonitorExitReason::SemihostingExit(Ok(_))) => {
            // On success, we only print if the user asked for it.
            (always_print_stacktrace, Ok("Firmware exited successfully"))
        }
        Ok(MonitorExitReason::UserExit) => {
            // On ctrl-c, we only print if the user asked for it.
            (always_print_stacktrace, Ok("Exited by user request"))
        }
        Ok(MonitorExitReason::Halted(halt_reason)) => {
            let reason = describe_halt_reason(halt_reason).to_string();
            (true, Err(FirmwareExited::Unexpected(reason).into()))
        }
        Ok(MonitorExitReason::SemihostingExit(Err(details))) => match details {
            // A deliberate exit, so like a successful one, only print a stack trace on request.
            SemihostingExitError {
                reason: SEMIHOSTING_APPLICATION_EXIT,
                subcode: Some(status),
            } => (
                always_print_stacktrace,
                Err(FirmwareExited::WithStatus(status).into()),
            ),
            SemihostingExitError { reason, .. } => (
                true,
                Err(FirmwareExited::WithReason(describe_semihosting_exit(reason)).into()),
            ),
        },
        Err(e) => {
            // Some irrecoverable error happened, probably can't print the stack trace.
            (false, Err(e.into()))
        }
    }
}

/// The semihosting exit reason of `exit(status)`.
const SEMIHOSTING_APPLICATION_EXIT: u32 = 0x20026;

/// Describes a semihosting exit reason that carries no exit status.
fn describe_semihosting_exit(reason: u32) -> String {
    let text = match reason {
        // HW vector reason codes
        0x20000 => "Branch through zero",
        0x20001 => "Undefined instruction",
        0x20002 => "Software interrupt",
        0x20003 => "Prefetch abort",
        0x20004 => "Data abort",
        0x20005 => "Address exception",
        0x20006 => "IRQ",
        0x20007 => "FIQ",
        // SW reason codes
        0x20020 => "Breakpoint",
        0x20021 => "Watchpoint",
        0x20022 => "Step complete",
        0x20023 => "Unknown runtime error",
        0x20024 => "Internal error",
        0x20025 => "User interruption",
        SEMIHOSTING_APPLICATION_EXIT => "Application exit",
        0x20027 => "Stack overflow",
        0x20028 => "Division by zero",
        0x20029 => "OS specific error",
        other => return format!("Unknown exit reason {other}"),
    };
    text.to_string()
}

/// Prints the stack trace, or why it can not be printed.
async fn try_display_stack_trace(
    session: &SessionInterface,
    path: Option<&Path>,
    stack_frame_limit: u32,
) {
    let Some(path) = path else {
        eprintln!("Can not print stack trace because firmware is not available");
        return;
    };
    if let Err(error) = display_stack_trace(session, path, stack_frame_limit).await {
        eprintln!("Can not print stack trace: {error}");
    }
}

/// The firmware exited with a non-zero status through semihosting.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum FirmwareExited {
    /// Firmware exited with status {0}
    WithStatus(u32),

    /// Firmware exited: {0}
    WithReason(String),

    /// Firmware stopped unexpectedly: {0}
    Unexpected(String),
}

impl FirmwareExited {
    /// Statuses that don't fit in an exit code map to 1, so a failure never reads as success.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::WithStatus(status) => match u8::try_from(*status) {
                Ok(0) | Err(_) => 1,
                Ok(code) => code,
            },
            _ => 1,
        }
    }
}

/// Describes why the core halted, for a user who runs firmware and does not
/// debug it.
fn describe_halt_reason(reason: WireHaltReason) -> &'static str {
    match reason {
        WireHaltReason::Multiple => "the core halted for multiple reasons",
        WireHaltReason::Breakpoint(WireBreakpointCause::Hardware) => {
            "the core halted on a hardware breakpoint"
        }
        WireHaltReason::Breakpoint(WireBreakpointCause::Software) => {
            "the core halted on a software breakpoint"
        }
        WireHaltReason::Breakpoint(WireBreakpointCause::Unknown) => {
            "the core halted on a breakpoint"
        }
        WireHaltReason::Breakpoint(WireBreakpointCause::Semihosting(_)) => {
            "the core halted on an unsupported semihosting command"
        }
        WireHaltReason::Exception => "the core halted on an exception",
        WireHaltReason::Watchpoint => "the core halted on a watchpoint",
        WireHaltReason::Step => "the core halted after a single step",
        WireHaltReason::Request => "the core halted on a debugger request",
        WireHaltReason::External => "the core halted on an external request",
        WireHaltReason::Unknown => "the core halted for an unknown reason",
    }
}

struct ChannelInfoPrinter<'a>(&'a ChannelInfo);

impl<'a> std::fmt::Display for ChannelInfoPrinter<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (buffer size {})", self.0.name, self.0.buffer_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_exit_code() {
        assert_eq!(FirmwareExited::WithStatus(1).exit_code(), 1);
        assert_eq!(FirmwareExited::WithStatus(42).exit_code(), 42);
        assert_eq!(FirmwareExited::WithStatus(255).exit_code(), 255);
        // Must not be reported as success
        assert_eq!(FirmwareExited::WithStatus(256).exit_code(), 1);
        assert_eq!(FirmwareExited::WithStatus(0).exit_code(), 1);
        assert_eq!(FirmwareExited::WithReason(String::from("")).exit_code(), 1);
        assert_eq!(FirmwareExited::Unexpected(String::from("")).exit_code(), 1);
    }

    #[test]
    fn firmware_exit_status_through_anyhow() {
        let error = anyhow::Error::from(FirmwareExited::WithStatus(3)).context("while running");
        let status = error.downcast_ref::<FirmwareExited>().unwrap();
        assert_eq!(status.exit_code(), 3);
    }
}
