use std::{num::NonZeroU32, time::Duration};

use crate::{
    rpc::{
        Key,
        functions::{MonitorEndpoint, MonitorTopic, RpcSpawnContext, WireTxImpl, flash::BootInfo},
        utils::run_loop::{RunLoop, RunLoopPoller},
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
use tokio::sync::mpsc;

#[derive(Serialize, Deserialize, Schema)]
pub enum MonitorMode {
    AttachToRunning,
    Run(BootInfo),
}

#[derive(Serialize, Deserialize, Schema)]
pub enum MonitorEvent {
    RttOutput { channel: u32, bytes: Vec<u8> },
    SemihostingOutput(SemihostingOutput),
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

pub async fn monitor(
    mut ctx: RpcSpawnContext,
    header: VarHeader,
    request: MonitorRequest,
    sender: Sender<WireTxImpl>,
) {
    let resp = ctx
        .run_blocking::<MonitorTopic, _, _, _>(request, monitor_impl)
        .await
        .map_err(Into::into);

    sender
        .reply::<MonitorEndpoint>(header.seq_no, &resp)
        .await
        .unwrap();
}

fn monitor_impl(
    ctx: RpcSpawnContext,
    request: MonitorRequest,
    sender: mpsc::Sender<MonitorEvent>,
) -> anyhow::Result<()> {
    let mut session = ctx.session_blocking(request.sessid);

    let mut semihosting_sink =
        MonitorEventHandler::new(|event| sender.blocking_send(event).unwrap());

    let mut rtt_client = request
        .options
        .rtt_client
        .map(|rtt_client| ctx.object_mut_blocking(rtt_client));

    let core_id = rtt_client.as_ref().map(|rtt| rtt.core_id()).unwrap_or(0);

    let mut run_loop = RunLoop {
        core_id,
        cancellation_token: ctx.cancellation_token(),
    };

    let poller = rtt_client.as_deref_mut().map(|client| RttPoller {
        rtt_client: client,
        sender: sender.clone(),
    });

    let monitor_mode = if session.core(core_id)?.core_halted()? {
        request.mode
    } else {
        // Core is running so we can ignore BootInfo
        MonitorMode::AttachToRunning
    };

    match monitor_mode {
        MonitorMode::Run(BootInfo::FromRam {
            vector_table_addr, ..
        }) => {
            // core should be already reset and halt by this point.
            session.prepare_running_on_ram(vector_table_addr)?;
        }
        MonitorMode::Run(BootInfo::Other) => {
            // reset the core to leave it in a consistent state after flashing
            session
                .core(core_id)?
                .reset_and_halt(Duration::from_millis(100))?;
        }
        MonitorMode::AttachToRunning => {
            // do nothing
        }
    }

    let mut core = session.core(run_loop.core_id)?;
    run_loop.run_until(
        &mut core,
        request.options.catch_hardfault,
        request.options.catch_reset,
        poller,
        None,
        |halt_reason, core| semihosting_sink.handle_halt(halt_reason, core),
    )?;

    Ok(())
}

pub struct RttPoller<'c> {
    pub rtt_client: &'c mut RttClient,
    pub sender: mpsc::Sender<MonitorEvent>,
}

impl RunLoopPoller for RttPoller<'_> {
    fn poll(&mut self, core: &mut Core<'_>) -> anyhow::Result<Duration> {
        if !self.rtt_client.is_attached() && matches!(self.rtt_client.try_attach(core), Ok(true)) {
            tracing::debug!("Attached to RTT");
        }

        let mut next_poll = Duration::from_millis(100);
        for channel in 0..self.rtt_client.up_channels().len() {
            let bytes = self.rtt_client.poll_channel(core, channel as u32)?;
            if !bytes.is_empty() {
                // Poll RTT with a frequency of 10 Hz if we do not receive any new data.
                // Once we receive new data, we bump the frequency to 1kHz.
                next_poll = Duration::from_millis(1);

                self.sender
                    .blocking_send(MonitorEvent::RttOutput {
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

struct MonitorEventHandler<F: FnMut(MonitorEvent)> {
    sender: F,
    semihosting_reader: SemihostingReader,
}

impl<F: FnMut(MonitorEvent)> MonitorEventHandler<F> {
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
    ) -> anyhow::Result<Option<()>> {
        let HaltReason::Breakpoint(BreakpointCause::Semihosting(cmd)) = halt_reason else {
            anyhow::bail!("CPU halted unexpectedly.");
        };

        match cmd {
            SemihostingCommand::ExitSuccess => Ok(Some(())), // Exit the run loop
            SemihostingCommand::ExitError(details) => {
                anyhow::bail!("Semihosting indicated exit with {details}")
            }
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
            other @ (SemihostingCommand::Open(_)
            | SemihostingCommand::Close(_)
            | SemihostingCommand::WriteConsole(_)
            | SemihostingCommand::Write(_)) => {
                if let Some(output) = self.semihosting_reader.handle(other, core)? {
                    (self.sender)(MonitorEvent::SemihostingOutput(output));
                }
                Ok(None)
            }
        }
    }
}

#[derive(Serialize, Deserialize, Schema)]
pub enum SemihostingOutput {
    StdOut(String),
    StdErr(String),
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

    pub fn handle(
        &mut self,
        command: SemihostingCommand,
        core: &mut Core<'_>,
    ) -> anyhow::Result<Option<SemihostingOutput>> {
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
                Some(SemihostingOutput::StdOut(str))
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
    ) -> anyhow::Result<Option<SemihostingOutput>> {
        match request.file_handle() {
            handle if handle == Self::STDOUT.get() => {
                if self.stdout_open {
                    let string = read_written_string(core, request)?;
                    return Ok(Some(SemihostingOutput::StdOut(string)));
                }
            }
            handle if handle == Self::STDERR.get() => {
                if self.stderr_open {
                    let string = read_written_string(core, request)?;
                    return Ok(Some(SemihostingOutput::StdErr(string)));
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
