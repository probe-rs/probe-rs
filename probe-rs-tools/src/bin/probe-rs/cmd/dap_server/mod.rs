// Bad things happen to the VSCode debug extension and debug_adapter if we panic at the wrong time.
#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::panic, clippy::expect_used)
)]
pub(crate) mod backend;
pub(crate) mod debug_adapter;
pub(crate) mod server;

#[cfg(test)]
mod test;

use anyhow::Result;
use probe_rs::{
    CoreDumpError, Error, architecture::arm::ap::AccessPortError, flashing::FileDownloadError,
    probe::DebugProbeError,
};
use probe_rs_debug::DebugError;
use server::startup::{debug_stdio, debug_tcp};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
};
use time::UtcOffset;

use crate::util::common_options::OperationError;
use probe_rs_rpc_client::RpcClient;

#[derive(Debug, thiserror::Error)]
pub enum DebuggerError {
    #[error(transparent)]
    AccessPort(#[from] AccessPortError),
    #[error("Failed to parse argument '{argument}'.")]
    ArgumentParseError {
        argument_index: usize,
        argument: String,
        source: anyhow::Error,
    },
    #[error(transparent)]
    DebugProbe(#[from] DebugProbeError),
    #[error(transparent)]
    DebugError(#[from] DebugError),
    #[error(transparent)]
    FileDownload(#[from] FileDownloadError),
    #[error("Received an invalid request")]
    InvalidRequest,
    #[error("Command requires a value for argument '{argument_name}'")]
    MissingArgument { argument_name: String },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
    #[error(transparent)]
    ProbeRs(#[from] Error),
    #[error(transparent)]
    OperationError(#[from] OperationError),
    /// Errors related to the handling of core dumps.
    #[error("An error with a CoreDump occurred")]
    CoreDump(#[from] CoreDumpError),
    #[error("{0}")]
    /// A user-facing message that does not unwind nested errors.
    UserMessage(String),
    #[error("Serialization error")]
    SerdeError(#[from] serde_json::Error),
    #[error(transparent)]
    StdIO(#[from] std::io::Error),
    #[error("Unable to open probe{}", .0.map(|s| format!(": {s}")).as_deref().unwrap_or("."))]
    UnableToOpenProbe(Option<&'static str>),
    #[error("Request not implemented")]
    Unimplemented,
}

/// Open target in debug mode and accept debug commands.
/// This only works as a [debug_adapter::protocol::DapAdapter] and uses Debug Adapter Protocol (DAP) commands (enables connections from clients such as Microsoft Visual Studio Code).
///
/// The DAP client talks to the server over stdin/stdout, unless `--ip`/`--port` make the server
/// listen for TCP connections. The server accesses the debug probes through an in-process probe-rs
/// server, unless `--host` points it at a remote probe-rs server.
#[derive(clap::Parser)]
pub struct Cmd {
    /// IP port number to listen for incoming DAP connections, e.g. "50000".
    /// When omitted, the DAP server communicates over stdin/stdout.
    #[clap(long)]
    port: Option<u16>,

    /// IP address to listen for incoming DAP connections, e.g. "127.0.0.1".
    /// Defaults to "127.0.0.1".
    #[clap(long, requires = "port")]
    ip: Option<IpAddr>,

    /// Some editors and IDEs expect the debug adapter processes to exit at the end of every debug
    /// session (on receiving a `Disconnect` or `Terminate` request).
    ///
    /// OTHERWISE probe-rs will persist and continue to listen for new DAP client connections
    /// ("multi-session" mode), and it becomes the user's responsibility to terminate the debug
    /// adapter process.
    ///
    /// Implied when `--port` is omitted (stdio mode).
    #[clap(long, alias("vscode"))]
    single_session: bool,
}

impl Cmd {
    /// True when the DAP server listens on TCP (`--port` was given).
    pub(crate) fn is_tcp_mode(&self) -> bool {
        self.port.is_some()
    }
}

pub async fn run(
    cmd: Cmd,
    stdio_client: Option<RpcClient>,
    remote: probe_rs_rpc_client::RemoteParams,
    time_offset: UtcOffset,
    log_file: Option<&Path>,
) -> Result<()> {
    match cmd.port {
        Some(port) => {
            let ip = cmd.ip.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
            let addr = SocketAddr::new(ip, port);
            debug_tcp(remote, addr, cmd.single_session, log_file, time_offset).await
        }
        None => {
            let Some(client) = stdio_client else {
                anyhow::bail!("stdio DAP mode requires an RPC client");
            };
            debug_stdio(client, log_file, time_offset).await
        }
    }
}
