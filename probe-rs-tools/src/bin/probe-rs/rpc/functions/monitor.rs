use std::{num::NonZeroU32, time::Duration};

use crate::{
    rpc::{
        Key,
        functions::{
            MonitorEndpoint, MultiTopicPublisher, MultiTopicWriter, RpcResult, RpcSpawnContext,
            RttTopic, SemihostingTopic, WireTxImpl, flash::BootInfo,
        },
        utils::run_loop::{ReturnReason, RunLoop, RunLoopPoller},
    },
    util::rtt::client::RttClient,
};
use anyhow::Context;
use postcard_rpc::{header::VarHeader, server::Sender};
use postcard_schema::Schema;
use probe_rs::{
    BreakpointCause, Core, HaltReason, Session,
    semihosting::{CloseRequest, OpenRequest, SemihostingCommand, WriteRequest},
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, error::SendError};
use tokio_util::sync::CancellationToken;

#[derive(Serialize, Deserialize, Schema)]
pub enum MonitorMode {
    AttachToRunning,
    Run(BootInfo),
}

impl MonitorMode {
    pub fn should_clear_rtt_header(&self) -> bool {
        match self {
            MonitorMode::Run(BootInfo::FromRam { .. }) => true,
            MonitorMode::Run(BootInfo::Other) => true,
            MonitorMode::AttachToRunning => false,
        }
    }

    pub fn prepare(&self, session: &mut Session, core_id: usize) -> anyhow::Result<()> {
        match self {
            MonitorMode::Run(boot_info) => boot_info.prepare(session, core_id),
            MonitorMode::AttachToRunning => Ok(()),
        }
    }
}

#[derive(Serialize, Deserialize, Schema)]
pub struct MonitorOptions {
    /// Enable reset vector catch if its supported on the target.
    pub catch_reset: bool,
    /// Enable hardfault vector catch if its supported on the target.
    pub catch_hardfault: bool,
    /// RTT client if used.
    pub rtt_client: Option<Key<RttClient>>,
}

/// Monitor in normal run mode.
#[derive(Serialize, Deserialize, Schema)]
pub struct MonitorRequest {
    pub sessid: Key<Session>,
    pub mode: MonitorMode,
    pub options: MonitorOptions,
}

/// Reasons why the firmware exited.
#[derive(Serialize, Deserialize, Schema)]
pub enum MonitorExitReason {
    Success,
    UserExit,
    SemihostingExit(Result<(), SemihostingExitError>),
    UnexpectedExit(String),
}

/// Details of an unexpected exit, triggered by a semihosting call.
#[derive(Serialize, Deserialize, Schema)]
pub struct SemihostingExitError {
    /// The reason for the exit.
    pub reason: u32,
    /// The subcode of the exit, if the call was EXIT_EXTENDED.
    pub subcode: Option<u32>,
}

/// If a communication error occurs, an error is returned. If we detect that the firmware exited,
/// a `MonitorExitReason` is returned.
pub type MonitorResponse = RpcResult<MonitorExitReason>;

pub async fn monitor(
    mut ctx: RpcSpawnContext,
    header: VarHeader,
    request: MonitorRequest,
    sender: Sender<WireTxImpl>,
) {
    let resp = ctx
        .run_blocking::<MonitorSender, _, _, _>(request, monitor_impl)
        .await
        .map_err(Into::into);

    sender
        .reply::<MonitorEndpoint>(header.seq_no, &resp)
        .await
        .unwrap();
}

#[derive(Serialize, Deserialize, Schema)]
pub enum RttEvent {
    Discovered {
        up_channels: Vec<String>,
        down_channels: Vec<String>,
    },
    Output {
        channel: u32,
        bytes: Vec<u8>,
    },
}

#[derive(Serialize, Deserialize, Schema)]
pub enum SemihostingEvent {
    Output { stream: String, data: String },
}

pub(crate) struct MonitorSender {
    rtt: mpsc::Sender<RttEvent>,
    semihosting_output: mpsc::Sender<SemihostingEvent>,
}
impl MonitorSender {
    pub(crate) fn send_semihosting_event(
        &self,
        event: SemihostingEvent,
    ) -> Result<(), SendError<SemihostingEvent>> {
        self.semihosting_output.blocking_send(event)
    }

    pub(crate) fn send_rtt_event(&self, event: RttEvent) -> Result<(), SendError<RttEvent>> {
        self.rtt.blocking_send(event)
    }
}

pub(crate) struct MonitorPublisher {
    rtt: <RttTopic as MultiTopicWriter>::Publisher,
    semihosting_output: <SemihostingTopic as MultiTopicWriter>::Publisher,
}

impl MultiTopicWriter for MonitorSender {
    type Sender = Self;
    type Publisher = MonitorPublisher;

    fn create(token: CancellationToken) -> (Self::Sender, Self::Publisher) {
        let (rtt_sender, rtt_publisher) = RttTopic::create(token.clone());
        let (semihosting_sender, semihosting_publisher) = SemihostingTopic::create(token);

        (
            Self {
                rtt: rtt_sender,
                semihosting_output: semihosting_sender,
            },
            MonitorPublisher {
                rtt: rtt_publisher,
                semihosting_output: semihosting_publisher,
            },
        )
    }
}

impl MultiTopicPublisher for MonitorPublisher {
    async fn publish(self, sender: &Sender<WireTxImpl>) {
        tokio::join!(
            self.rtt.publish(sender),
            self.semihosting_output.publish(sender)
        );
    }
}

fn monitor_impl(
    ctx: RpcSpawnContext,
    request: MonitorRequest,
    sender: MonitorSender,
) -> anyhow::Result<MonitorExitReason> {
    let mut session = ctx.session_blocking(request.sessid);

    let mut semihosting_sink =
        MonitorEventHandler::new(|event| sender.send_semihosting_event(event).unwrap());

    let mut rtt_client = request
        .options
        .rtt_client
        .map(|rtt_client| ctx.object_mut_blocking(rtt_client));

    let core_id = rtt_client.as_ref().map(|rtt| rtt.core_id()).unwrap_or(0);

    let mut run_loop = RunLoop {
        core_id,
        cancellation_token: ctx.cancellation_token(),
    };

    request.mode.prepare(&mut session, run_loop.core_id)?;

    let mut core = session.core(run_loop.core_id)?;
    if request.mode.should_clear_rtt_header() {
        if let Some(rtt_client) = rtt_client.as_mut() {
            rtt_client.clear_control_block(&mut core)?;
        }
    }

    let poller = rtt_client.as_deref_mut().map(|client| RttPoller {
        rtt_client: client,
        sender: |message| {
            sender
                .send_rtt_event(message)
                .context("Failed to send RTT event")
        },
    });

    let exit_reason = run_loop.run_until(
        &mut core,
        request.options.catch_hardfault,
        request.options.catch_reset,
        poller,
        None,
        |halt_reason, core| semihosting_sink.handle_halt(halt_reason, core),
    )?;

    match exit_reason {
        ReturnReason::Predicate(reason) => Ok(reason),
        ReturnReason::Timeout => anyhow::bail!("Run loop exited due to an unexpected timeout"),
        ReturnReason::Cancelled => Ok(MonitorExitReason::UserExit),
        ReturnReason::LockedUp => anyhow::bail!("Run loop exited due to a locked up core"),
    }
}

pub struct RttPoller<'c, S>
where
    S: FnMut(RttEvent) -> anyhow::Result<()>,
    S: 'c,
{
    pub rtt_client: &'c mut RttClient,
    pub sender: S,
}

impl<'c, S> RunLoopPoller for RttPoller<'c, S>
where
    S: FnMut(RttEvent) -> anyhow::Result<()>,
    S: 'c,
{
    fn start(&mut self, _core: &mut Core<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    fn poll(&mut self, core: &mut Core<'_>) -> anyhow::Result<Duration> {
        if !self.rtt_client.is_attached() && matches!(self.rtt_client.try_attach(core), Ok(true)) {
            tracing::debug!("Attached to RTT");
            let up_channels = self
                .rtt_client
                .up_channels()
                .iter()
                .map(|c| c.channel_name())
                .collect::<Vec<_>>();
            let down_channels = self
                .rtt_client
                .down_channels()
                .iter()
                .map(|c| c.channel_name())
                .collect::<Vec<_>>();
            (self.sender)(RttEvent::Discovered {
                up_channels,
                down_channels,
            })
            .with_context(|| "Failed to send RTT discovery")?;
        }

        let mut next_poll = Duration::from_millis(100);
        for channel in 0..self.rtt_client.up_channels().len() {
            let bytes = self.rtt_client.poll_channel(core, channel as u32)?;
            if !bytes.is_empty() {
                // Poll RTT with a frequency of 10 Hz if we do not receive any new data.
                // Once we receive new data, we bump the frequency to 1kHz.
                next_poll = Duration::from_millis(1);

                (self.sender)(RttEvent::Output {
                    channel: channel as u32,
                    bytes: bytes.to_vec(),
                })
                .with_context(|| "Failed to send RTT output")?;
            }
        }

        Ok(next_poll)
    }

    fn exit(&mut self, core: &mut Core<'_>) -> anyhow::Result<()> {
        self.rtt_client.clean_up(core)?;
        Ok(())
    }
}

struct MonitorEventHandler<F: FnMut(SemihostingEvent)> {
    sender: F,
    semihosting_reader: SemihostingReader,
}

impl<F: FnMut(SemihostingEvent)> MonitorEventHandler<F> {
    pub fn new(sender: F) -> Self {
        Self {
            sender,
            semihosting_reader: SemihostingReader::new(),
        }
    }

    fn handle_halt(
        &mut self,
        halt_reason: HaltReason,
        core: &mut Core<'_>,
    ) -> anyhow::Result<Option<MonitorExitReason>> {
        let HaltReason::Breakpoint(BreakpointCause::Semihosting(cmd)) = halt_reason else {
            return Ok(Some(MonitorExitReason::UnexpectedExit(format!(
                "{:?}",
                halt_reason
            ))));
        };

        match cmd {
            SemihostingCommand::ExitSuccess => Ok(Some(MonitorExitReason::SemihostingExit(Ok(())))), // Exit the run loop
            SemihostingCommand::ExitError(details) => Ok(Some(MonitorExitReason::SemihostingExit(
                Err(SemihostingExitError {
                    reason: details.reason,
                    subcode: details.exit_status.or(details.subcode),
                }),
            ))),
            SemihostingCommand::Unknown(details) => {
                tracing::warn!(
                    "Target wanted to run semihosting operation {:#x} with parameter {:#x},\
                     but probe-rs does not support this operation yet. Continuing...",
                    details.operation,
                    details.parameter
                );
                Ok(None) // Continue running
            }
            SemihostingCommand::GetCommandLine(_) => {
                tracing::warn!(
                    "Target wanted to run semihosting operation SYS_GET_CMDLINE, but probe-rs does not support this operation yet. Continuing..."
                );
                Ok(None) // Continue running
            }
            SemihostingCommand::Errno(_) => Ok(None),
            other if SemihostingReader::is_io(other) => {
                if let Some((stream, data)) = self.semihosting_reader.handle(other, core)? {
                    (self.sender)(SemihostingEvent::Output { stream, data });
                }
                Ok(None)
            }
            other => Ok(Some(MonitorExitReason::UnexpectedExit(format!(
                "Unexpected semihosting command {:?}",
                other,
            )))),
        }
    }
}

pub struct SemihostingReader {
    stdout_open: bool,
    stderr_open: bool,
}

impl SemihostingReader {
    const STDOUT: NonZeroU32 = NonZeroU32::new(1).unwrap();
    const STDERR: NonZeroU32 = NonZeroU32::new(2).unwrap();

    pub fn new() -> Self {
        Self {
            stdout_open: false,
            stderr_open: false,
        }
    }

    pub fn is_io(other: SemihostingCommand) -> bool {
        matches!(
            other,
            SemihostingCommand::Open(_)
                | SemihostingCommand::Close(_)
                | SemihostingCommand::WriteConsole(_)
                | SemihostingCommand::Write(_)
        )
    }

    pub fn handle(
        &mut self,
        command: SemihostingCommand,
        core: &mut Core<'_>,
    ) -> anyhow::Result<Option<(String, String)>> {
        let out = match command {
            SemihostingCommand::Open(request) => {
                self.handle_open(core, request)?;
                None
            }
            SemihostingCommand::Close(request) => {
                self.handle_close(core, request)?;
                None
            }
            SemihostingCommand::Write(request) => self.handle_write(core, request)?,
            SemihostingCommand::WriteConsole(request) => {
                let str = request.read(core)?;
                Some((String::from("stdout"), str))
            }

            _ => None,
        };

        Ok(out)
    }

    fn handle_open(&mut self, core: &mut Core<'_>, request: OpenRequest) -> anyhow::Result<()> {
        let path = request.path(core)?;
        if path != ":tt" {
            tracing::warn!(
                "Target wanted to open file {path}, but probe-rs does not support this operation yet. Continuing..."
            );
            return Ok(());
        }

        match request.mode().as_bytes()[0] {
            b'w' => {
                self.stdout_open = true;
                request.respond_with_handle(core, Self::STDOUT)?;
            }
            b'a' => {
                self.stderr_open = true;
                request.respond_with_handle(core, Self::STDERR)?;
            }
            mode => tracing::warn!(
                "Target wanted to open file {path} with mode {mode}, but probe-rs does not support this operation yet. Continuing..."
            ),
        }

        Ok(())
    }

    fn handle_close(&mut self, core: &mut Core<'_>, request: CloseRequest) -> anyhow::Result<()> {
        let handle = request.file_handle(core)?;
        if handle == Self::STDOUT.get() {
            self.stdout_open = false;
            request.success(core)?;
        } else if handle == Self::STDERR.get() {
            self.stderr_open = false;
            request.success(core)?;
        } else {
            tracing::warn!(
                "Target wanted to close file handle {handle}, but probe-rs does not support this operation yet. Continuing..."
            );
        }

        Ok(())
    }

    fn handle_write(
        &mut self,
        core: &mut Core<'_>,
        request: WriteRequest,
    ) -> anyhow::Result<Option<(String, String)>> {
        match request.file_handle() {
            handle if handle == Self::STDOUT.get() => {
                if self.stdout_open {
                    let string = read_written_string(core, request)?;
                    return Ok(Some((String::from("stdout"), string)));
                }
            }
            handle if handle == Self::STDERR.get() => {
                if self.stderr_open {
                    let string = read_written_string(core, request)?;
                    return Ok(Some((String::from("stderr"), string)));
                }
            }
            other => tracing::warn!(
                "Target wanted to write to file handle {other}, but probe-rs does not support this operation yet. Continuing...",
            ),
        }

        Ok(None)
    }
}

fn read_written_string(core: &mut Core<'_>, request: WriteRequest) -> anyhow::Result<String> {
    let bytes = request.read(core)?;
    let str = String::from_utf8_lossy(&bytes);
    request.write_status(core, 0)?;
    Ok(str.to_string())
}
