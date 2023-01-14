// Bad things happen to the VSCode debug extenison and debug_adapter if we panic at the wrong time.
#![warn(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
// Uses Schemafy to generate DAP types from Json
mod debug_adapter;
mod debugger;
mod peripherals;

use anyhow::{Context, Result};
use chrono::Local;
use clap::{crate_authors, crate_description, crate_name, crate_version, Parser};
use debugger::debug_entry::{debug, list_connected_devices, list_supported_chips};
use probe_rs::{
    architecture::arm::ap::AccessPortError, flashing::FileDownloadError, DebugProbeError, Error,
};
use std::{env::var, fs::File, io::stderr};
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
    #[error("Missing session for interaction with probe")]
    MissingSession,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
    #[error(transparent)]
    ProbeRs(#[from] Error),
    #[error("Serialiazation error")]
    SerdeError(#[from] serde_json::Error),
    #[error("Failed to open source file '{source_file_name}'.")]
    ReadSourceError {
        source_file_name: String,
        original_error: std::io::Error,
    },
    #[error("IO error: '{original_error}'.")]
    NonBlockingReadError { original_error: std::io::Error },
    #[error(transparent)]
    StdIO(#[from] std::io::Error),
    #[error("Unable to open probe{}", .0.map(|s| format!(": {}", s)).as_deref().unwrap_or("."))]
    UnableToOpenProbe(Option<&'static str>),
    #[error("Request not implemented")]
    Unimplemented,
}

/// CliCommands enum contains the list of supported commands that can be invoked from the command line.
#[derive(clap::Parser)]
#[clap(
    name = crate_name!(),
    about = crate_description!(),
    author = crate_authors!(),
    version = crate_version!()
)]

/// There are only 3 command line options for the debugger.
enum CliCommands {
    /// List all connected debug probes
    List {},
    /// List all probe-rs supported chips
    #[clap(name = "list-chips")]
    ListChips {},
    /// Open target in debug mode and accept debug commands.
    /// This only works as a [debug_adapter::protocol::DapAdapter] and uses DAP Protocol debug commands (enables connections from clients such as Microsoft Visual Studio Code).
    Debug {
        /// IP port number to listen for incoming DAP connections, e.g. "50000"
        #[clap(long)]
        port: Option<u16>,

        /// The debug adapter processed was launched by VSCode, and should terminate itself at the end of every debug session (when receiving `Disconnect` or `Terminate` Request from VSCode). The "false"(default) state of this option implies that the process was launched (and will be managed) by the user.
        #[clap(long, hide = true)]
        vscode: bool,
    },
}

fn main() -> Result<()> {
    let log_info_message = setup_logging()?;

    let matches = CliCommands::parse();

    match matches {
        CliCommands::List {} => list_connected_devices()?,
        CliCommands::ListChips {} => list_supported_chips()?,
        CliCommands::Debug { port, vscode } => debug(port, vscode, &log_info_message)?,
    }
    Ok(())
}

/// Setup logging, according to the following rules.
/// 1. If the RUST_LOG environment variable is set, use it as a `LevelFilter` to configure a subscriber that logs to a file in the system's application data directory.
/// 2. Irrespective of the RUST_LOG environment variable, configure a subscribe that will write with `LevelFilter::ERROR` to stderr, because these errors are picked up and reported to the user by the VSCode extension.
fn setup_logging() -> Result<String, anyhow::Error> {
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
                format!("{}.log", Local::now().timestamp_millis()),
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
