use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::rpc::{
    ObjectStorageSlot, SharedTargetSerial,
    functions::{
        MultiTopicPublisher, MultiTopicWriter, RpcContext, RpcSpawnContext, WireTxImpl,
        core_ops::convert,
    },
    utils::{
        run_loop::{ReturnReason, RunLoop, RunLoopPoller, VectorCatchConfig},
        semihosting::SemihostingFileManager,
    },
};
use anyhow::Context;
use postcard_rpc::{header::VarHeader, server::Sender};
use probe_rs::{
    BreakpointCause, Core, HaltReason, MemoryInterface, semihosting::SemihostingCommand,
};
use probe_rs_rpc::monitor::{
    ChannelInfo, MonitorExitReason, MonitorMode, MonitorRequest, RttEvent, SemihostingEvent,
    SemihostingExitError, SerialEvent, SerialWriteRequest, SerialWriteResponse,
};
use probe_rs_rpc::semihosting_options::SemihostingOptions;
use probe_rs_rpc::{MonitorEndpoint, RpcError, RttTopic, SemihostingTopic, SerialTopic};
use tokio::sync::mpsc::{self, error::SendError};
use tokio_util::sync::CancellationToken;

fn prepare_monitor_mode(
    mode: &MonitorMode,
    session: &mut probe_rs::Session,
    core_id: usize,
) -> anyhow::Result<()> {
    match mode {
        MonitorMode::Run(boot_info) => {
            crate::rpc::functions::flash::prepare_boot_info(boot_info, session, core_id)
        }
        MonitorMode::AttachToRunning => Ok(()),
    }
}

pub async fn monitor(
    mut ctx: RpcSpawnContext,
    header: VarHeader,
    request: MonitorRequest,
    sender: Sender<WireTxImpl>,
) {
    let resp = ctx
        .run_blocking::<MonitorSender, _, _, _>(request, monitor_impl)
        .await
        .map_err(crate::rpc::functions::convert::rpc_error_anyhow);

    sender
        .reply::<MonitorEndpoint>(header.seq_no, &resp)
        .await
        .unwrap();
}

pub(crate) struct MonitorSender {
    rtt: mpsc::Sender<RttEvent>,
    semihosting_output: mpsc::Sender<SemihostingEvent>,
    serial: mpsc::Sender<SerialEvent>,
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

    pub(crate) fn send_serial_event(
        &self,
        event: SerialEvent,
    ) -> Result<(), SendError<SerialEvent>> {
        self.serial.blocking_send(event)
    }
}

pub(crate) struct MonitorPublisher {
    rtt: <RttTopic as MultiTopicWriter>::Publisher,
    semihosting_output: <SemihostingTopic as MultiTopicWriter>::Publisher,
    serial: <SerialTopic as MultiTopicWriter>::Publisher,
}

impl MultiTopicWriter for MonitorSender {
    type Sender = Self;
    type Publisher = MonitorPublisher;

    fn create(token: CancellationToken) -> (Self::Sender, Self::Publisher) {
        let (rtt_sender, rtt_publisher) = RttTopic::create(token.clone());
        let (semihosting_sender, semihosting_publisher) = SemihostingTopic::create(token.clone());
        let (serial_sender, serial_publisher) = SerialTopic::create(token);

        (
            Self {
                rtt: rtt_sender,
                semihosting_output: semihosting_sender,
                serial: serial_sender,
            },
            MonitorPublisher {
                rtt: rtt_publisher,
                semihosting_output: semihosting_publisher,
                serial: serial_publisher,
            },
        )
    }
}

impl MultiTopicPublisher for MonitorPublisher {
    async fn publish(self, sender: &Sender<WireTxImpl>) {
        tokio::join!(
            self.rtt.publish(sender),
            self.semihosting_output.publish(sender),
            self.serial.publish(sender)
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

    let target_serial = shared_session.target_serial();
    if let (MonitorMode::Run(_), Some(serial)) = (&request.mode, &target_serial) {
        // The port is open from attach. Drop the output from before this boot.
        discard_serial_input(serial);
    }

    {
        let mut session = shared_session.session_blocking();
        prepare_monitor_mode(&request.mode, &mut session, run_loop.core_id)?;
    }

    let rtt_poller = client_key.map(|client| RttPoller {
        rtt_client: shared_session.object_storage().cell(client),
        clear_control_block: request.mode.should_clear_rtt_header(),
        sender: |message| {
            sender
                .send_rtt_event(message)
                .context("Failed to send RTT event")
        },
    });

    let exit_reason = with_serial_forwarding(target_serial.as_ref(), &sender, || {
        run_loop.run_until(
            &shared_session,
            VectorCatchConfig {
                catch_hardfault: request.options.catch_hardfault,
                catch_reset: request.options.catch_reset,
                catch_svc: request.options.catch_svc,
                catch_hlt: request.options.catch_hlt,
            },
            rtt_poller,
            None,
            |halt_reason, core| semihosting_sink.handle_halt(halt_reason, core),
        )
    })?;

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
    pub rtt_client: ObjectStorageSlot<crate::util::rtt::client::RttClient>,
    pub clear_control_block: bool,
    pub sender: S,
}

impl<S> RunLoopPoller for RttPoller<S>
where
    S: FnMut(RttEvent) -> anyhow::Result<()>,
{
    fn start(&mut self, core: &mut impl MemoryInterface) -> anyhow::Result<()> {
        if self.clear_control_block {
            let mut rtt_client = self.rtt_client.get_blocking();
            rtt_client.clear_control_block(core)?;
        }
        Ok(())
    }

    fn memory_port(&mut self, core: &Core) -> Option<usize> {
        self.rtt_client.get_blocking().memory_port(core)
    }

    fn poll(&mut self, core: &mut impl MemoryInterface) -> anyhow::Result<Duration> {
        let mut rtt_client = self.rtt_client.get_blocking();
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
        rtt_client.warn_if_control_block_is_missing();

        let mut next_poll = Duration::from_millis(100);
        for channel in 0..rtt_client.up_channels().len() {
            let bytes = rtt_client.poll_channel(core, channel as u32)?;
            if !bytes.is_empty() {
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

    fn exit(&mut self, core: &mut impl MemoryInterface) -> anyhow::Result<()> {
        let mut rtt_client = self.rtt_client.get_blocking();
        rtt_client.clean_up(core)?;
        Ok(())
    }
}

/// How long one read of the serial port to the target waits before the stop flag is checked.
const SERIAL_READ_TIMEOUT: Duration = Duration::from_millis(10);
/// After the stop, the data that arrives in this time is still sent.
const SERIAL_TAIL_TIME: Duration = Duration::from_millis(50);

/// Runs `f` and sends the data from the serial port to the target while `f` runs. The serial port
/// does not need the session, so a thread reads it next to `f`.
pub(crate) fn with_serial_forwarding<R>(
    serial: Option<&SharedTargetSerial>,
    sender: &MonitorSender,
    f: impl FnOnce() -> R,
) -> R {
    /// Sets the flag when `f` returns or panics, so that the thread stops.
    struct StopOnDrop<'a>(&'a AtomicBool);
    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        if let Some(serial) = serial {
            scope.spawn(|| forward_serial(serial, &stop, sender));
        }
        let _stop = StopOnDrop(&stop);
        f()
    })
}

/// Drops the data that the serial port to the target received before the target boots.
pub(crate) fn discard_serial_input(serial: &SharedTargetSerial) {
    if let Err(error) = serial.discard_input() {
        tracing::debug!("Failed to discard the input of the serial port to the target: {error}");
    }
}

/// Sends the data from the serial port to the target until `stop` is set, then sends the data
/// that arrives shortly after.
fn forward_serial(serial: &SharedTargetSerial, stop: &AtomicBool, sender: &MonitorSender) {
    let mut buf = [0; 4096];
    let mut stop_at = None;

    while stop_at.is_none_or(|stop_at| Instant::now() < stop_at) {
        if stop_at.is_none() && stop.load(Ordering::Relaxed) {
            // Data can still be on the way, for example the end of a panic message.
            stop_at = Some(Instant::now() + SERIAL_TAIL_TIME);
        }

        let len = match serial.read(&mut buf, SERIAL_READ_TIMEOUT) {
            Ok(len) => len,
            Err(error) => {
                tracing::debug!("Failed to read the serial port to the target: {error}");
                return;
            }
        };
        if len > 0
            && sender
                .send_serial_event(SerialEvent::Output {
                    bytes: buf[..len].to_vec(),
                })
                .is_err()
        {
            return;
        }
    }
}

/// Writes data to the serial port to the target.
pub async fn write_serial(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: SerialWriteRequest,
) -> SerialWriteResponse {
    let serial = ctx
        .target_serial(request.sessid)
        .ok_or_else(|| RpcError::from("The session has no serial port to the target"))?;
    // A write can wait for the target, so it must not block the async runtime.
    let written = tokio::task::spawn_blocking(move || serial.write(&request.data))
        .await
        .unwrap()
        .map_err(|error| RpcError::from(error.to_string()))?;
    Ok(written as u32)
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
            return Ok(Some(MonitorExitReason::Halted(
                convert::to_wire_halt_reason(halt_reason),
            )));
        };

        match cmd {
            SemihostingCommand::ExitSuccess => Ok(Some(MonitorExitReason::SemihostingExit(Ok(())))),
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
                Ok(None)
            }
            SemihostingCommand::GetCommandLine(_) => {
                tracing::warn!(
                    "Target wanted to run semihosting operation SYS_GET_CMDLINE, but probe-rs does not support this operation yet. Continuing..."
                );
                Ok(None)
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
            other => Ok(Some(MonitorExitReason::Halted(
                convert::to_wire_halt_reason(HaltReason::Breakpoint(BreakpointCause::Semihosting(
                    other,
                ))),
            ))),
        }
    }
}
