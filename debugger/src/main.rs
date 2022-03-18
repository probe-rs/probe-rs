// Bad things happen to the VSCode debug extenison and debug_adapter if we panic at the wrong time.
#![warn(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
// Uses Schemafy to generate DAP types from Json
mod debug_adapter;
mod debugger;

use anyhow::Result;
use clap::{crate_authors, crate_description, crate_name, crate_version, Parser};
use debugger::debug_entry::{debug, list_connected_devices, list_supported_chips};
use probe_rs::{
    architecture::arm::ap::AccessPortError, flashing::FileDownloadError, DebugProbeError, Error,
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
    /// This only works as a [protocol::DapAdapter] and uses DAP Protocol debug commands (enables connections from clients such as Microsoft Visual Studio Code).
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
    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Stderr) // Log to Stderr, so that VSCode Debug Extension can intercept the messages and pass them to the VSCode DAP Client
        .init();

    let matches = CliCommands::parse();

    match matches {
        CliCommands::List {} => list_connected_devices()?,
        CliCommands::ListChips {} => list_supported_chips()?,
        CliCommands::Debug { port, vscode } => debug(port, vscode)?,
    }
    Ok(())
}
