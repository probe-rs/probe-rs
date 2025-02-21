use std::{num::NonZeroU32, time::Duration};

use crate::{
    rpc::{
        functions::{flash::BootInfo, MonitorEndpoint, MonitorTopic, RpcSpawnContext, WireTxImpl},
        utils::run_loop::RunLoop,
        Key,
    },
    util::rtt::client::RttClient,
};
use anyhow::Context;
use postcard_rpc::{header::VarHeader, server::Sender};
use postcard_schema::Schema;
use probe_rs::{semihosting::SemihostingCommand, BreakpointCause, Core, HaltReason, Session};
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
        rtt_client: rtt_client.as_deref_mut(),
        cancellation_token: ctx.cancellation_token(),
    };

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
        |channel, bytes| {
            sender
                .blocking_send(MonitorEvent::RttOutput { channel, bytes })
                .with_context(|| "Failed to send RTT output")
        },
        None,
        |halt_reason, core| semihosting_sink.handle_halt(halt_reason, core),
    )?;

    Ok(())
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
                Err(anyhow::anyhow!("Semihosting indicated exit with {details}"))
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
                tracing::warn!("Target wanted to run semihosting operation SYS_GET_CMDLINE, but probe-rs does not support this operation yet. Continuing...");
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
                let path = request.path(core)?;
                if path == ":tt" {
                    match request.mode().as_bytes()[0] {
                        b'w' => {
                            self.stdout_open = true;
                            request.respond_with_handle(core, Self::STDOUT)?;
                        }
                        b'a' => {
                            self.stderr_open = true;
                            request.respond_with_handle(core, Self::STDERR)?;
                        }
                        other => {
                            tracing::warn!(
                                "Target wanted to open file {path} with mode {mode}, but probe-rs does not support this operation yet. Continuing...",
                                path = path,
                                mode = other
                            );
                        }
                    };
                } else {
                    tracing::warn!(
                        "Target wanted to open file {path}, but probe-rs does not support this operation yet. Continuing..."
                    );
                }
                None
            }
            SemihostingCommand::Close(request) => {
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
                None
            }
            SemihostingCommand::Write(request) => {
                let mut out = None;
                match request.file_handle() {
                    handle if handle == Self::STDOUT.get() => {
                        if self.stdout_open {
                            let bytes = request.read(core)?;
                            let str = String::from_utf8_lossy(&bytes);
                            out = Some(SemihostingOutput::StdOut(str.to_string()));
                            request.write_status(core, 0)?;
                        }
                    }
                    handle if handle == Self::STDERR.get() => {
                        if self.stderr_open {
                            let bytes = request.read(core)?;
                            let str = String::from_utf8_lossy(&bytes);
                            out = Some(SemihostingOutput::StdErr(str.to_string()));
                            request.write_status(core, 0)?;
                        }
                    }
                    other => {
                        tracing::warn!(
                            "Target wanted to write to file handle {other}, but probe-rs does not support this operation yet. Continuing...",
                        );
                    }
                }
                out
            }
            SemihostingCommand::WriteConsole(request) => {
                let str = request.read(core)?;
                Some(SemihostingOutput::StdOut(str))
            }

            _ => None,
        };

        Ok(out)
    }
}
