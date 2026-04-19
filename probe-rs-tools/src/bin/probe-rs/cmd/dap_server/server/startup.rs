use super::debugger::Debugger;
use crate::cmd::dap_server::debug_adapter::{dap::adapter::*, protocol::DapAdapter};
use crate::rpc::client::RpcClient;
use anyhow::{Context, Result};
use probe_rs::{config::Registry, probe::list::Lister};
use serde::Deserialize;
use std::{
    fs,
    io::{self, Read},
    net::TcpListener,
    path::Path,
    sync::mpsc,
    time::{Duration, UNIX_EPOCH},
};
use time::UtcOffset;

#[derive(Copy, Clone, Debug, Deserialize, PartialEq)]
pub(crate) enum TargetSessionType {
    AttachRequest,
    LaunchRequest,
}

impl std::str::FromStr for TargetSessionType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_ascii_lowercase()[..] {
            "attach" => Ok(TargetSessionType::AttachRequest),
            "launch" => Ok(TargetSessionType::LaunchRequest),
            _ => Err(format!(
                "'{s}' is not a valid target session type. Can be either 'attach' or 'launch']."
            )),
        }
    }
}

pub async fn debug_tcp(
    client: RpcClient,
    lister: &Lister,
    addr: std::net::SocketAddr,
    single_session: bool,
    log_file: Option<&Path>,
    timestamp_offset: UtcOffset,
) -> Result<()> {
    let mut debugger = Debugger::new(timestamp_offset, log_file)?;

    let old_hook = std::panic::take_hook();
    let logger = debugger.debug_logger.clone();
    std::panic::set_hook(Box::new(move |panic_info| {
        // Flush logs before printing panic.
        _ = logger.flush();
        old_hook(panic_info);
    }));

    loop {
        let listener = TcpListener::bind(addr)?;

        debugger
            .debug_logger
            .log_to_console(&format!("Listening for requests on port {}", addr.port()))?;

        if !single_session {
            // When running as a server from the command line, we want startup logs to go to the stderr.
            debugger.debug_logger.flush()?;
        }

        listener.set_nonblocking(false)?;

        match listener.accept() {
            Ok((socket, addr)) => {
                socket.set_nonblocking(true).with_context(|| {
                    format!("Failed to negotiate non-blocking socket with request from: {addr}")
                })?;

                debugger
                    .debug_logger
                    .log_to_console(&format!("Starting debug session from: {addr}"))?;

                let reader = socket
                    .try_clone()
                    .context("Failed to establish a bi-directional Tcp connection.")?;
                let writer = socket;

                let dap_adapter = DapAdapter::new(reader, writer);
                let mut debug_adapter = DebugAdapter::new(dap_adapter);

                // Flush any pending log messages to the debug adapter Console Log.
                debugger.debug_logger.flush_to_dap(&mut debug_adapter)?;

                let mut registry = Registry::from_builtin_families();
                let end_message = match run_debug_session(
                    &mut debugger,
                    &client,
                    &mut registry,
                    debug_adapter,
                    lister,
                )
                .await
                {
                    // We no longer have a reference to the `debug_adapter`, so errors need
                    // special handling to ensure they are displayed to the user.
                    Err(error) => {
                        eprintln!("Session ended with error: {error:?}");
                        format!("Session ended: {error}")
                    }
                    Ok(()) => format!("Closing debug session from: {addr}"),
                };
                debugger.debug_logger.log_to_console(&end_message)?;

                // Terminate after a single debug session. This is the behaviour expected by VSCode
                // if it started probe-rs as a child process.
                if single_session {
                    break;
                }
            }
            Err(error) => {
                tracing::error!(
                    "probe-rs-debugger failed to establish a socket connection. Reason: {:?}",
                    error
                );
            }
        }
        debugger.debug_logger.flush()?;
    }

    debugger
        .debug_logger
        .log_to_console("DAP Protocol server exiting")?;

    debugger.debug_logger.flush()?;

    Ok(())
}

/// Non-blocking reader backed by an `mpsc::Receiver<Vec<u8>>`.
///
/// Returns `io::ErrorKind::WouldBlock` when the channel is empty (no data
/// available yet) and `Ok(0)` when the sender has disconnected (EOF).
pub(crate) struct ChannelReader {
    rx: mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl ChannelReader {
    pub(crate) fn new(rx: mpsc::Receiver<Vec<u8>>) -> Self {
        Self {
            rx,
            buf: Vec::new(),
            pos: 0,
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        // Drain any leftover buffered data first.
        if self.pos < self.buf.len() {
            let n = std::cmp::min(out.len(), self.buf.len() - self.pos);
            out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }

        // Try to receive a new chunk from the channel.
        match self.rx.try_recv() {
            Ok(chunk) => {
                let n = std::cmp::min(out.len(), chunk.len());
                out[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.buf = chunk;
                    self.pos = n;
                } else {
                    self.buf.clear();
                    self.pos = 0;
                }
                Ok(n)
            }
            Err(mpsc::TryRecvError::Empty) => {
                Err(io::Error::new(io::ErrorKind::WouldBlock, "no data yet"))
            }
            Err(mpsc::TryRecvError::Disconnected) => Ok(0),
        }
    }
}

pub async fn debug_stdio(
    client: RpcClient,
    lister: &Lister,
    log_file: Option<&Path>,
    timestamp_offset: UtcOffset,
) -> Result<()> {
    let mut debugger = Debugger::new(timestamp_offset, log_file)?;

    let old_hook = std::panic::take_hook();
    let logger = debugger.debug_logger.clone();
    std::panic::set_hook(Box::new(move |panic_info| {
        _ = logger.flush();
        old_hook(panic_info);
    }));

    debugger
        .debug_logger
        .log_to_console("Starting stdio DAP session")?;

    // Spawn a background thread to read stdin and forward chunks over a channel.
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut handle = stdin.lock();
        let mut buf = [0u8; 4096];
        loop {
            match handle.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let reader = ChannelReader::new(rx);
    let writer = io::stdout();

    let dap_adapter = DapAdapter::new(reader, writer);
    let mut debug_adapter = DebugAdapter::new(dap_adapter);

    debugger.debug_logger.flush_to_dap(&mut debug_adapter)?;

    let mut registry = Registry::from_builtin_families();
    match run_debug_session(
        &mut debugger,
        &client,
        &mut registry,
        debug_adapter,
        lister,
    )
    .await
    {
        Err(error) => {
            eprintln!("Session ended with error: {error:?}");
            debugger
                .debug_logger
                .log_to_console(&format!("Session ended: {error}"))?;
        }
        Ok(()) => {
            debugger
                .debug_logger
                .log_to_console("Closing stdio DAP session")?;
        }
    }

    debugger
        .debug_logger
        .log_to_console("DAP Protocol server exiting")?;
    debugger.debug_logger.flush()?;

    Ok(())
}

/// Pick the correct [`Debugger`] entry point based on whether the provided
/// [`RpcClient`] is backed by an in-process RPC server (local session) or a
/// real remote connection.
///
/// In local mode the debugger keeps using the historical `Session` backend so
/// that direct probe access stays available. In remote mode every operation
/// is proxied through the RPC layer via [`crate::cmd::dap_server::backend::rpc::RpcBackend`].
async fn run_debug_session<P>(
    debugger: &mut Debugger,
    client: &RpcClient,
    registry: &mut Registry,
    debug_adapter: DebugAdapter<P>,
    lister: &Lister,
) -> Result<(), crate::cmd::dap_server::DebuggerError>
where
    P: crate::cmd::dap_server::debug_adapter::protocol::ProtocolAdapter,
{
    if client.is_local_session() {
        debugger
            .debug_session(registry, debug_adapter, lister)
            .await
    } else {
        debugger.debug_session_rpc(client, debug_adapter).await
    }
}

/// Try to get the timestamp of a file.
///
/// If an error occurs, None is returned.
pub(crate) fn get_file_timestamp(path_to_elf: &Path) -> Option<Duration> {
    fs::metadata(path_to_elf)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
}
