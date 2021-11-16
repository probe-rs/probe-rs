// Uses Schemafy to generate DAP types from Json
mod dap_types;
mod debug_adapter;
mod debugger;
mod info;
mod protocol;
mod rtt;

use anyhow::Result;
use debugger::{
    debug, download_program_fast, dump_memory, list_connected_devices, list_supported_chips,
    reset_target_of_device, trace_u32_on_target, DebuggerOptions,
};
use probe_rs::architecture::arm::ap::AccessPortError;
use probe_rs::flashing::FileDownloadError;
use probe_rs::{DebugProbeError, Error};
use structopt::clap::{crate_authors, crate_description, crate_name, crate_version};
use structopt::StructOpt;

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
    // #[error("Error in interaction with probe")]
    // ProbeError(#[from] probe_rs::Error),
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

/* Some helper functions for StructOpt parsing */
fn parse_hex(src: &str) -> Result<u32, std::num::ParseIntError> {
    u32::from_str_radix(src, 16)
}
// fn parse_server(src: &str) -> Result<SocketAddr, AddrParseError> {
//     src.parse()
// }

/// CliCommands enum contains the list of supported commands that can be invoked from the command line.
/// The `debug` command is also the entry point for the DAP server, when the --dap option is used.
#[derive(StructOpt)]
#[structopt(
    name = crate_name!(),
    about = crate_description!(),
    author = crate_authors!(),
    version = crate_version!()
)]
enum CliCommands {
    /// List all connected debug probes
    #[structopt(name = "list")]
    List {},
    /// List all probe-rs supported chips
    #[structopt(name = "list-chips")]
    ListChips {},
    /// Gets infos about the selected debug probe and connected target
    #[structopt(name = "info")]
    Info {
        #[structopt(flatten)]
        debugger_options: DebuggerOptions,
    },
    /// Resets the target attached to the selected debug probe
    #[structopt(name = "reset")]
    Reset {
        #[structopt(flatten)]
        debugger_options: DebuggerOptions,

        /// Whether the reset pin should be asserted or deasserted. If left open, just pulse it
        assert: Option<bool>,
    },
    /// Open target in debug mode and accept debug commands.
    /// By default, the program operates in CLI mode.
    #[structopt(name = "debug")]
    Debug {
        #[structopt(flatten)]
        debugger_options: DebuggerOptions,

        /// Switch from using the CLI(command line interface) to using DAP Protocol debug commands (enables connections from clients such as Microsoft Visual Studio Code).
        /// This option requires the user to specify the `port` option, along with a valid IP port number on which the server will listen for incoming connections.
        #[structopt(long)]
        dap: bool,

        /// The debug adapter processed was launched by VSCode, and should terminate itself at the end of every debug session (when receiving `Disconnect` or `Terminate` Request from VSCode). The "false"(default) state of this option implies that the process was launched (and will be managed) by the user.
        #[structopt(long, hidden = true, requires("dap"))]
        vscode: bool,
    },
    /// Dump memory from attached target
    #[structopt(name = "dump")]
    Dump {
        #[structopt(flatten)]
        debugger_options: DebuggerOptions,

        /// The address of the memory to dump from the target (in hexadecimal without 0x prefix)
        #[structopt(parse(try_from_str = parse_hex))]
        loc: u32,
        /// The amount of memory (in words) to dump
        words: u32,
    },
    /// Download memory to attached target
    #[structopt(name = "download")]
    Download {
        #[structopt(flatten)]
        debugger_options: DebuggerOptions,

        /// The path to the file to be downloaded to the flash
        path: String,
    },
    /// Begin tracing a memory address over SWV
    #[structopt(name = "trace")]
    Trace {
        #[structopt(flatten)]
        debugger_options: DebuggerOptions,

        /// The address of the memory start trace (in hexadecimal without 0x prefix)
        #[structopt(parse(try_from_str = parse_hex))]
        loc: u32,
    },
}

fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Stderr) // Log to Stderr, so that VSCode Debug Extension can intercept the messages and pass them to the VSCode DAP Client
        .init();

    let matches = CliCommands::from_args();

    match matches {
        CliCommands::List {} => list_connected_devices()?,
        CliCommands::ListChips {} => list_supported_chips()?,
        CliCommands::Info { debugger_options } => {
            crate::info::show_info_of_device(&debugger_options)?
        }
        CliCommands::Reset {
            debugger_options,
            assert,
        } => reset_target_of_device(debugger_options, assert)?,
        CliCommands::Debug {
            debugger_options,
            dap,
            vscode,
        } => debug(debugger_options, dap, vscode)?,
        CliCommands::Dump {
            debugger_options,
            loc,
            words,
        } => dump_memory(debugger_options, loc, words)?,
        CliCommands::Download {
            debugger_options,
            path,
        } => download_program_fast(debugger_options, &path)?,
        CliCommands::Trace {
            debugger_options,
            loc,
        } => trace_u32_on_target(debugger_options, loc)?,
    }
    Ok(())
}
