// Bad things happen to the VSCode debug extenison and debug_adapter if we panic at the wrong time.
#![warn(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
// Uses Schemafy to generate DAP types from Json
mod debug_adapter;
mod peripherals;
mod server;

#[cfg(test)]
mod test;

use anyhow::Result;
use probe_rs::{
    architecture::arm::ap::AccessPortError, flashing::FileDownloadError, probe::list::Lister,
    probe::DebugProbeError, CoreDumpError, Error,
};
use server::startup::debug;
use std::{fs::File, io::stderr, path::Path};
use time::UtcOffset;
use tracing::metadata::LevelFilter;
use tracing_subscriber::{
    fmt::format::FmtSpan, prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt,
    EnvFilter, Layer,
};

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
    let log_info_message = setup_logging(log_file)?;

    debug(
        lister,
        cmd.port,
        cmd.single_session,
        &log_info_message,
        time_offset,
    )
}

/// Setup logging, according to the following rules.
/// 1. If the RUST_LOG environment variable is set, use it as a `LevelFilter` to configure a subscriber that logs to a file in the system's application data directory.
/// 2. Irrespective of the RUST_LOG environment variable, configure a subscribe that will write with `LevelFilter::ERROR` to stderr, because these errors are picked up and reported to the user by the VSCode extension.
///
/// Determining the local time for logging purposes can fail, so it needs to be given as a parameter here.
fn setup_logging(log_file: Option<&Path>) -> Result<String, anyhow::Error> {
    // We want to always log errors to stderr, but not to the log file.
    let stderr_subscriber = tracing_subscriber::fmt::layer()
        .compact()
        .with_ansi(false)
        .without_time()
        .with_writer(stderr)
        .with_filter(LevelFilter::ERROR);

    match log_file {
        Some(log_path) => {
            let log_file = File::create(log_path)?;

            // The log file will respect the RUST_LOG environment variable as a filter.
            let file_subscriber = tracing_subscriber::fmt::layer()
                .json()
                .with_file(true)
                .with_line_number(true)
                .with_span_events(FmtSpan::FULL)
                .with_writer(log_file)
                .with_filter(EnvFilter::from_default_env());

            tracing_subscriber::registry()
                .with(stderr_subscriber)
                .with(file_subscriber)
                .init();

            Ok(format!(
                "Log output will be written to: {:?}",
                log_path.display()
            ))
        }
        None => {
            tracing_subscriber::registry()
                .with(stderr_subscriber)
                .init();
            Ok("No logging data will be written because the RUST_LOG environment variable is not set.".to_string())
        }
    }
}
