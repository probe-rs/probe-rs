mod dap_types; //Uses Schemafy to generate DAP types from Json
mod debug_adapter;
mod debugger; //The probe-rs debugger.
mod info;
mod rtt;

use anyhow::Result;
use debugger::{
    debug, download_program_fast, dump_memory, list_connected_devices, reset_target_of_device,
    trace_u32_on_target, DebuggerOptions,
};
use log::error;
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
    NonBlockingReadError {
        os_error_number: i32,
        original_error: std::io::Error,
    },
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

        //TODO: Implement multi-session --server choices
        /// Switch from using CLI to DAP Protocol debug commands. By default, the DAP communication for the first session is via STDIN and STDOUT. Adding the additional --port property will run as an IP server, listening to connections on the specified port.
        #[structopt(long)]
        dap: bool,
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
    //TODO: Consider using https://github.com/probe-rs/probe-rs/blob/master/probe-rs-cli-util/src/logging.rs
    //TODO: See if we can have a single solution for RUST_LOG and the DAP Client Console Log (`debug_adapter::log_to_console`)
    // Initialize the logging backend.
    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Stderr) // Log to Stderr, because the DebugAdapater, in 'Launch' mode, needs Stdin and Stdout to communicate with VSCode DAP Client
        .init();

    let matches = CliCommands::from_args();

    //TODO: Fix all the unwrap() and ?'s
    match matches {
        CliCommands::List {} => list_connected_devices()?,
        CliCommands::Info { debugger_options } => {
            crate::info::show_info_of_device(&debugger_options)?
        }
        CliCommands::Reset {
            debugger_options,
            assert,
        } => reset_target_of_device(debugger_options, assert)?,
        CliCommands::Debug {
            debugger_options,
            // program_binary,
            // port,
            dap,
        } => debug(debugger_options, dap),
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
