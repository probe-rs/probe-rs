// Bad things happen to the VSCode debug extenison and debug_adapter if we panic at the wrong time.
#![warn(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
// Uses Schemafy to generate DAP types from Json
mod debug_adapter;
mod peripherals;
mod server;

#[cfg(test)]
mod test;

use anyhow::{Context, Result};
use probe_rs::{
    architecture::arm::ap::AccessPortError, flashing::FileDownloadError, CoreDumpError,
    DebugProbeError, Error, Lister,
};
use server::startup::debug;
use std::{env::var, fs::File, io::stderr};
use time::{OffsetDateTime, UtcOffset};
use tracing::metadata::LevelFilter;
use tracing_subscriber::{
    fmt::format::FmtSpan, prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt,
    EnvFilter, Layer,
};

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

pub fn run(cmd: Cmd, lister: &Lister, time_offset: UtcOffset) -> Result<()> {
    let log_info_message = setup_logging(time_offset)?;

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
fn setup_logging(time_offset: UtcOffset) -> Result<String, anyhow::Error> {
    // We want to always log errors to stderr, but not to the log file.
    let stderr_subscriber = tracing_subscriber::fmt::layer()
        .compact()
        .with_ansi(false)
        .without_time()
        .with_writer(stderr)
        .with_filter(LevelFilter::ERROR);

    match var("RUST_LOG") {
        Ok(rust_log) => {
            let project_dirs = directories::ProjectDirs::from("rs", "probe-rs", "probe-rs")
                .context("Could not determine the application storage directory required for the log output files.")?;
            let directory = project_dirs.data_dir();
            let logname = sanitize_filename::sanitize_with_options(
                format!(
                    "{}.log",
                    OffsetDateTime::now_utc()
                        .to_offset(time_offset)
                        .unix_timestamp_nanos()
                        / 1_000_000
                ),
                sanitize_filename::Options {
                    replacement: "_",
                    ..Default::default()
                },
            );
            std::fs::create_dir_all(directory)
                .context(format!("{directory:?} could not be created"))?;
            let log_path = directory.join(logname);
            let log_file = File::create(&log_path)?;
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
                "\"RUST_LOG={}\" output will be written to: {:?}",
                rust_log,
                log_path.to_string_lossy()
            ))
        }
        Err(_) => {
            tracing_subscriber::registry()
                .with(stderr_subscriber)
                .init();
            Ok("No logging data will be written because the RUST_LOG environment variable is not set.".to_string())
        }
    }
}
