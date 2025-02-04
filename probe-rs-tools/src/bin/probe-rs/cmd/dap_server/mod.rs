// Bad things happen to the VSCode debug extenison and debug_adapter if we panic at the wrong time.
#![warn(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod debug_adapter;
mod peripherals;
mod server;

#[cfg(test)]
mod test;

use anyhow::Result;
use probe_rs::{
    architecture::arm::ap_v1::AccessPortError,
    flashing::FileDownloadError,
    probe::{list::Lister, DebugProbeError},
    CoreDumpError, Error,
};
use probe_rs_debug::DebugError;
use server::startup::debug;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
};
use time::UtcOffset;

use crate::util::common_options::OperationError;

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
    #[error("Received an invalid requeset")]
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
    #[error("An error with a CoreDump occured")]
    CoreDump(#[from] CoreDumpError),
    #[error("{0}")]
    /// A message that is intended to be displayed to the user, and does not unwind nested errors.
    /// It is intended to communicate helpful "correct and try again" information to users.
    UserMessage(String),
    #[error("Serialization error")]
    SerdeError(#[from] serde_json::Error),
    #[error("IO error: '{original_error}'.")]
    NonBlockingReadError { original_error: std::io::Error },
    #[error(transparent)]
    StdIO(#[from] std::io::Error),
    #[error("Unable to open probe{}", .0.map(|s| format!(": {s}")).as_deref().unwrap_or("."))]
    UnableToOpenProbe(Option<&'static str>),
    #[error("Request not implemented")]
    Unimplemented,
}

/// Open target in debug mode and accept debug commands.
/// This only works as a [debug_adapter::protocol::DapAdapter] and uses Debug Adapter Protocol (DAP) commands (enables connections from clients such as Microsoft Visual Studio Code).
#[derive(clap::Parser)]
pub struct Cmd {
    /// IP port number to listen for incoming DAP connections, e.g. "50000"
    #[clap(long)]
    port: u16,

    /// IP address to listen for incoming DAP connections, e.g. "127.0.0.1"
    #[clap(long, default_value_t = Ipv4Addr::LOCALHOST.into())]
    ip: IpAddr,

    /// Some editors and IDEs expect the debug adapter processes to exit at the end of every debug
    /// session (on receiving a `Disconnect` or `Terminate` request).
    ///
    /// OTHERWISE probe-rs will persist and continue to listen for new DAP client connections
    /// ("multi-session" mode), and it becomes the user's responsibility to terminate the debug
    /// adapter process.
    #[clap(long, alias("vscode"))]
    single_session: bool,
}

pub fn run(
    cmd: Cmd,
    lister: &Lister,
    time_offset: UtcOffset,
    log_file: Option<&Path>,
) -> Result<()> {
    let addr = SocketAddr::new(cmd.ip, cmd.port);
    debug(lister, addr, cmd.single_session, log_file, time_offset)
}
