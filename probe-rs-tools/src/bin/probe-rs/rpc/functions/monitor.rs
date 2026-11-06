use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::rpc::{
    Key, ObjectStorageSlot, RttClient, Session, SharedTargetSerial,
    functions::{
        MultiTopicPublisher, MultiTopicWriter, RpcContext, RpcSpawnContext, WireTxImpl,
        core_ops::convert,
        rtt_client::{RttDownStream, write_queued},
        write_queue::{Published, QUEUE_DEPTH, accept},
    },
    utils::{
        run_loop::{ReturnReason, RunLoop, RunLoopPoller, VectorCatchConfig},
        semihosting::SemihostingFileManager,
        semihosting_mappings::Mappings,
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
use probe_rs_rpc::{MonitorEndpoint, RpcError, RttTopic, SemihostingTopic, SerialTopic};
use tokio::sync::mpsc::{self, error::SendError};
use tokio_util::sync::DropGuard;

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

    crate::rpc::functions::reply::<MonitorEndpoint>(&sender, header, &resp).await;
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

    fn serial_events(&self) -> mpsc::Sender<SerialEvent> {
        self.serial.clone()
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

    fn create() -> (Self::Sender, Self::Publisher) {
        let (rtt_sender, rtt_publisher) = RttTopic::create();
        let (semihosting_sender, semihosting_publisher) = SemihostingTopic::create();
        let (serial_sender, serial_publisher) = SerialTopic::create();

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

    let mut semihosting_sink = MonitorEventHandler::new(
        Mappings::new(&ctx.semihosting_options(request.options.semihosting_options)?)?,
        |event| _ = sender.send_semihosting_event(event),
    );

    let client_key = request.options.rtt_client;
    let core_id = client_key
        .map(|rtt_client| ctx.object_mut_blocking(rtt_client).core_id())
        .unwrap_or(0);

    let mut run_loop = RunLoop {
        core_id,
        cancellation_token: ctx.cancellation_token(),
    };

    let serial_poller = SerialPoller::new(&ctx, request.sessid, &sender);
    if let (MonitorMode::Run(_), Some(serial)) = (&request.mode, &serial_poller) {
        // The port is open from attach. Drop the output from before this boot.
        serial.discard_input();
    }

    let rtt_poller = client_key.map(|client| {
        RttPoller::new(
            &ctx,
            client,
            request.mode.should_clear_rtt_header(),
            |message| {
                sender
                    .send_rtt_event(message)
                    .context("Failed to send RTT event")
            },
        )
    });
    let mut poller = (rtt_poller, serial_poller);
    run_loop.start_poller(&shared_session, &mut poller)?;

    {
        let mut session = shared_session.session_blocking();
        prepare_monitor_mode(&request.mode, &mut session, run_loop.core_id)?;
    }

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

/// Sends the data of the RTT up channels, and runs a worker for the writes to the down channels.
pub struct RttPoller<S>
where
    S: FnMut(RttEvent) -> anyhow::Result<()>,
{
    ctx: RpcSpawnContext,
    client: Key<RttClient>,
    rtt_client: ObjectStorageSlot<crate::util::rtt::client::RttClient>,
    clear_control_block: bool,
    sender: S,
    down_writes: Option<DownWrites>,
}

/// The published down channels of an [`RttPoller`]. Its drop stops the worker.
struct DownWrites {
    stream: Arc<RttDownStream>,
    _published: Published<Key<RttClient>, RttDownStream>,
    _stop: DropGuard,
}

impl<S> RttPoller<S>
where
    S: FnMut(RttEvent) -> anyhow::Result<()>,
{
    pub fn new(
        ctx: &RpcSpawnContext,
        client: Key<RttClient>,
        clear_control_block: bool,
        sender: S,
    ) -> Self {
        Self {
            ctx: ctx.clone(),
            client,
            rtt_client: ctx.state.object_storage.blocking_lock().cell(client),
            clear_control_block,
            sender,
            down_writes: None,
        }
    }
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
        let (stream, writes) = RttDownStream::new();
        let stream = Arc::new(stream);
        if let Some(published) = self.ctx.state.rtt_down.publish(self.client, stream.clone()) {
            let stop = self.ctx.cancellation_token().child_token();
            tokio::spawn(write_queued(self.ctx.state.clone(), writes, stop.clone()));
            self.down_writes = Some(DownWrites {
                stream,
                _published: published,
                _stop: stop.drop_guard(),
            });
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
        if let Some(down_writes) = &self.down_writes
            && rtt_client.is_attached()
        {
            down_writes
                .stream
                .set_channels(rtt_client.down_channels().len());
        }

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
        // The run loop holds the session, so the worker writes nothing after the clean-up.
        self.down_writes = None;
        let mut rtt_client = self.rtt_client.get_blocking();
        rtt_client.clean_up(core)?;
        Ok(())
    }
}

/// How long one read of the serial port to the target waits before the stop flag is checked.
const SERIAL_READ_TIMEOUT: Duration = Duration::from_millis(10);
/// After the stop, the data that arrives in this time is still sent.
const SERIAL_TAIL_TIME: Duration = Duration::from_millis(50);

/// Forwards the serial port to the target while a run loop runs. The serial port does not need
/// the session, so a thread reads it, and a worker writes the data that the client sends.
pub(crate) struct SerialPoller {
    ctx: RpcSpawnContext,
    sessid: Key<Session>,
    serial: SharedTargetSerial,
    events: mpsc::Sender<SerialEvent>,
    forwarding: Option<SerialForwarding>,
}

impl SerialPoller {
    /// Returns `None` if the session has no serial port to the target.
    pub(crate) fn new(
        ctx: &RpcSpawnContext,
        sessid: Key<Session>,
        sender: &MonitorSender,
    ) -> Option<Self> {
        Some(Self {
            ctx: ctx.clone(),
            sessid,
            serial: ctx.target_serial(sessid)?,
            events: sender.serial_events(),
            forwarding: None,
        })
    }

    /// Drops the data that the serial port received before the target boots.
    pub(crate) fn discard_input(&self) {
        if let Err(error) = self.serial.discard_input() {
            tracing::debug!(
                "Failed to discard the input of the serial port to the target: {error}"
            );
        }
    }
}

impl RunLoopPoller for SerialPoller {
    fn start(&mut self, _core: &mut impl MemoryInterface) -> anyhow::Result<()> {
        let stop = Arc::new(AtomicBool::new(false));
        let reader = thread::spawn({
            let serial = self.serial.clone();
            let stop = stop.clone();
            let events = self.events.clone();
            move || forward_serial(&serial, &stop, &events)
        });

        let (queue, writes) = mpsc::channel(QUEUE_DEPTH);
        let published = self
            .ctx
            .state
            .serial_writes
            .publish(self.sessid, Arc::new(queue));
        // Without a published queue, nothing sends to `writes`, so the writer ends at once.
        let writer = thread::spawn({
            let serial = self.serial.clone();
            let stop = stop.clone();
            move || write_serial_queued(&serial, &stop, writes)
        });

        self.forwarding = Some(SerialForwarding {
            stop,
            threads: vec![reader, writer],
            published,
        });
        Ok(())
    }

    fn poll(&mut self, _core: &mut impl MemoryInterface) -> anyhow::Result<Duration> {
        Ok(Duration::MAX)
    }

    fn exit(&mut self, _core: &mut impl MemoryInterface) -> anyhow::Result<()> {
        // The run loop holds the session here. The thread sends the data that arrives shortly
        // after, so the poller waits for it only when it drops.
        if let Some(forwarding) = &mut self.forwarding {
            forwarding.stop();
        }
        Ok(())
    }
}

/// The threads of a [`SerialPoller`]. Its drop stops them.
struct SerialForwarding {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    published: Option<Published<Key<Session>, mpsc::Sender<Vec<u8>>>>,
}

impl SerialForwarding {
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // The writer ends when the last sender of its queue drops.
        self.published = None;
    }
}

impl Drop for SerialForwarding {
    fn drop(&mut self) {
        self.stop();
        // The monitor sends its reply after the last serial event, and the session must not end
        // while a thread uses its serial port.
        for thread in self.threads.drain(..) {
            if thread.join().is_err() {
                tracing::debug!("A thread of the serial port to the target panicked");
            }
        }
    }
}

/// Sends the data from the serial port to the target until `stop` is set, then sends the data
/// that arrives shortly after.
fn forward_serial(
    serial: &SharedTargetSerial,
    stop: &AtomicBool,
    events: &mpsc::Sender<SerialEvent>,
) {
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
            && events
                .blocking_send(SerialEvent::Output {
                    bytes: buf[..len].to_vec(),
                })
                .is_err()
        {
            return;
        }
    }
}

/// Writes the queued writes until the queue closes. A write waits for flow control until the
/// target takes all of it, or until `stop` is set.
fn write_serial_queued(
    serial: &SharedTargetSerial,
    stop: &AtomicBool,
    mut writes: mpsc::Receiver<Vec<u8>>,
) {
    while let Some(data) = writes.blocking_recv() {
        let mut written = 0;
        while written < data.len() && !stop.load(Ordering::Relaxed) {
            match serial.write(&data[written..]) {
                Ok(len) => written += len,
                Err(error) => {
                    tracing::warn!("Failed to write to the serial port to the target: {error}");
                    break;
                }
            }
        }
    }
}

/// Writes data to the serial port to the target. While a run loop runs, the write goes to the
/// queue of the run loop, and the reply only says that the write is accepted.
pub async fn write_serial(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: SerialWriteRequest,
) -> SerialWriteResponse {
    if let Some(queue) = ctx.state.serial_writes.get(request.sessid) {
        let len = request.data.len();
        return accept(&queue, request.data, len);
    }

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
    pub fn new(mappings: Mappings, sender: F) -> Self {
        Self {
            semihosting_file_manager: SemihostingFileManager::new(mappings),
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicBool};
    use std::time::Duration;

    use probe_rs::probe::target_serial::{SerialSettings, TargetSerial, TargetSerialError};
    use tokio::sync::mpsc;

    use super::write_serial_queued;
    use crate::rpc::SharedTargetSerial;

    /// Takes at most two bytes for each write, like a port under flow control.
    #[derive(Debug, Default)]
    struct SlowPort(parking_lot::Mutex<Vec<u8>>);

    impl TargetSerial for SlowPort {
        fn default_settings(&self) -> SerialSettings {
            SerialSettings::default()
        }

        fn open(&mut self, _settings: &SerialSettings) -> Result<(), TargetSerialError> {
            Ok(())
        }

        fn read(&self, _buf: &mut [u8], _timeout: Duration) -> Result<usize, TargetSerialError> {
            Ok(0)
        }

        fn discard_input(&self) -> Result<(), TargetSerialError> {
            Ok(())
        }

        fn write(&self, data: &[u8]) -> Result<usize, TargetSerialError> {
            let len = data.len().min(2);
            self.0.lock().extend_from_slice(&data[..len]);
            Ok(len)
        }
    }

    /// Runs the writer on two queued writes until the queue closes.
    fn write(stop: bool) -> Vec<u8> {
        let port = Arc::new(SlowPort::default());
        let serial: SharedTargetSerial = port.clone();
        let (queue, writes) = mpsc::channel(8);
        queue.try_send(b"hello".to_vec()).unwrap();
        queue.try_send(b" world".to_vec()).unwrap();
        drop(queue);
        write_serial_queued(&serial, &AtomicBool::new(stop), writes);
        port.0.lock().clone()
    }

    #[test]
    fn a_queued_serial_write_is_written_in_full_and_in_order() {
        assert_eq!(write(false), b"hello world");
    }

    #[test]
    fn a_stopped_serial_writer_drops_its_queue() {
        assert!(write(true).is_empty());
    }
}
