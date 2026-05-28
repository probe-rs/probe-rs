use std::time::Duration;

use crate::{
    rpc::{
        Key, ObjectStorage,
        functions::{
            MonitorEndpoint, MultiTopicPublisher, MultiTopicWriter, RpcResult, RpcSpawnContext,
            RttTopic, SemihostingTopic, WireTxImpl, flash::BootInfo,
        },
        utils::{
            run_loop::{ReturnReason, RunLoop, RunLoopPoller, VectorCatchConfig},
            semihosting::{SemihostingFileManager, SemihostingOptions},
        },
    },
    util::rtt::client::RttClient,
};
use anyhow::Context;
use postcard_rpc::{header::VarHeader, server::Sender};
use postcard_schema::Schema;
use probe_rs::{BreakpointCause, Core, HaltReason, Session, semihosting::SemihostingCommand};
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
    /// Enable SVC vector catch (ARMv7-A/R only).
    pub catch_svc: bool,
    /// Enable HLT vector catch (ARMv7-A/R only).
    pub catch_hlt: bool,
    /// RTT client if used.
    pub rtt_client: Option<Key<RttClient>>,
    /// Configure the support for semihosting.
    pub semihosting_options: SemihostingOptions,
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

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct ChannelInfo {
    pub name: String,
    pub buffer_size: u64,
}

#[derive(Serialize, Deserialize, Schema)]
pub enum RttEvent {
    Discovered {
        up_channels: Vec<ChannelInfo>,
        down_channels: Vec<ChannelInfo>,
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
    let shared_session = ctx.shared_session(request.sessid);

    let mut semihosting_sink =
        MonitorEventHandler::new(request.options.semihosting_options, |event| {
            sender.send_semihosting_event(event).unwrap()
        });

    let client_key = request.options.rtt_client;
    let core_id = client_key
        .map(|rtt_client| ctx.object_mut_blocking(rtt_client).core_id())
        .unwrap_or(0);

    let mut run_loop = RunLoop {
        core_id,
        cancellation_token: ctx.cancellation_token(),
    };

    {
        let mut session = shared_session.session_blocking();
        request.mode.prepare(&mut session, run_loop.core_id)?;
    }

    let poller = client_key.map(|client| RttPoller {
        rtt_client: client,
        clear_control_block: request.mode.should_clear_rtt_header(),
        sender: |message| {
            sender
                .send_rtt_event(message)
                .context("Failed to send RTT event")
        },
    });

    let exit_reason = run_loop.run_until(
        &shared_session,
        VectorCatchConfig {
            catch_hardfault: request.options.catch_hardfault,
            catch_reset: request.options.catch_reset,
            catch_svc: request.options.catch_svc,
            catch_hlt: request.options.catch_hlt,
        },
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

pub struct RttPoller<S>
where
    S: FnMut(RttEvent) -> anyhow::Result<()>,
{
    pub rtt_client: Key<RttClient>,
    pub clear_control_block: bool,
    pub sender: S,
}

impl<S> RunLoopPoller for RttPoller<S>
where
    S: FnMut(RttEvent) -> anyhow::Result<()>,
{
    fn start(&mut self, objs: &ObjectStorage, core: &mut Core<'_>) -> anyhow::Result<()> {
        if self.clear_control_block {
            let mut rtt_client = objs.object_mut_blocking(self.rtt_client);
            rtt_client.clear_control_block(core)?;
        }
        Ok(())
    }

    fn poll(&mut self, objs: &ObjectStorage, core: &mut Core<'_>) -> anyhow::Result<Duration> {
        let mut rtt_client = objs.object_mut_blocking(self.rtt_client);
        if !rtt_client.is_attached() && matches!(rtt_client.try_attach(core), Ok(true)) {
            tracing::debug!("Attached to RTT");
            let up_channels = rtt_client
                .up_channels()
                .iter()
                .map(|c| ChannelInfo {
                    name: c.channel_name(),
                    buffer_size: c.buffer_size() as u64,
                })
                .collect::<Vec<_>>();
            let down_channels = rtt_client
                .down_channels()
                .iter()
                .map(|c| ChannelInfo {
                    name: c.channel_name(),
                    buffer_size: c.buffer_size() as u64,
                })
                .collect::<Vec<_>>();
            (self.sender)(RttEvent::Discovered {
                up_channels,
                down_channels,
            })
            .with_context(|| "Failed to send RTT discovery")?;
        }

        let mut next_poll = Duration::from_millis(100);
        for channel in 0..rtt_client.up_channels().len() {
            let bytes = rtt_client.poll_channel(core, channel as u32)?;
            if !bytes.is_empty() {
                // Poll RTT with a frequency of 10 Hz if we do not receive any new data.
                // Once we receive new data, we poll continuously while we have anything to read.
                next_poll = Duration::ZERO;

                (self.sender)(RttEvent::Output {
                    channel: channel as u32,
                    bytes: bytes.to_vec(),
                })
                .with_context(|| "Failed to send RTT output")?;
            }
        }

        Ok(next_poll)
    }

    fn exit(&mut self, objs: &ObjectStorage, core: &mut Core<'_>) -> anyhow::Result<()> {
        let mut rtt_client = objs.object_mut_blocking(self.rtt_client);
        rtt_client.clean_up(core)?;
        Ok(())
    }
}

struct MonitorEventHandler<F: FnMut(SemihostingEvent)> {
    semihosting_file_manager: SemihostingFileManager,
    sender: F,
}

impl<F: FnMut(SemihostingEvent)> MonitorEventHandler<F> {
    pub fn new(semihosting_options: SemihostingOptions, sender: F) -> Self {
        Self {
            semihosting_file_manager: SemihostingFileManager::new(semihosting_options),
            sender,
        }
    }

    fn handle_halt(
        &mut self,
        halt_reason: HaltReason,
        core: &mut Core<'_>,
    ) -> anyhow::Result<Option<MonitorExitReason>> {
        let HaltReason::Breakpoint(BreakpointCause::Semihosting(cmd)) = halt_reason else {
            return Ok(Some(MonitorExitReason::UnexpectedExit(format!(
                "{halt_reason:?}"
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
            SemihostingCommand::Time(request) => {
                request.write_current_time(core)?;
                Ok(None)
            }
            SemihostingCommand::Errno(_) => Ok(None),
            other if SemihostingFileManager::can_handle(other) => {
                self.semihosting_file_manager
                    .handle(other, core, &mut self.sender)?;
                Ok(None)
            }
            other => Ok(Some(MonitorExitReason::UnexpectedExit(format!(
                "Unexpected semihosting command {other:?}",
            )))),
        }
    }
}
