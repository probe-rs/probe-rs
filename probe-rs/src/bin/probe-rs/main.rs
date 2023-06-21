mod benchmark;
mod cargo_embed;
mod cargo_flash;
mod chip;
mod common;
mod dap_server;
mod debug;
mod download;
mod dump;
mod erase;
mod gdb;
mod info;
mod itm;
mod list;
mod reset;
mod run;
mod trace;
mod util;

include!(concat!(env!("OUT_DIR"), "/meta.rs"));

use std::path::Path;
use std::{ffi::OsString, fs::File, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use time::{OffsetDateTime, UtcOffset};
use tracing::metadata::LevelFilter;
use tracing_subscriber::{
    fmt::format::FmtSpan, prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt,
    EnvFilter, Layer,
};

#[derive(clap::Parser)]
#[clap(
    name = "probe-rs",
    about = "The probe-rs CLI",
    version = meta::CARGO_VERSION,
    long_version = meta::LONG_VERSION
)]
struct Cli {
    /// Location for log file
    ///
    /// If no location is specified, the log file will be stored in a default directory.
    #[clap(long, global = true)]
    log_file: Option<PathBuf>,
    #[clap(subcommand)]
    subcommand: Subcommand,
}

#[derive(clap::Subcommand)]
enum Subcommand {
    /// Debug Adapter Protocol (DAP) server. See https://probe.rs/docs/tools/vscode/
    DapServer(dap_server::Cmd),
    /// List all connected debug probes
    List(list::Cmd),
    /// Gets info about the selected debug probe and connected target
    Info(info::Cmd),
    /// Resets the target attached to the selected debug probe
    Reset(reset::Cmd),
    /// Run a GDB server
    Gdb(gdb::Cmd),
    /// Basic command line debugger
    Debug(debug::Cmd),
    /// Dump memory from attached target
    Dump(dump::Cmd),
    /// Download memory to attached target
    Download(download::Cmd),
    /// Erase all nonvolatile memory of attached target
    Erase(erase::Cmd),
    /// Flash and run an ELF program
    #[clap(name = "run")]
    Run(run::Cmd),
    /// Trace a memory location on the target
    #[clap(name = "trace")]
    Trace(trace::Cmd),
    /// Configure and monitor ITM trace packets from the target.
    #[clap(name = "itm")]
    Itm(itm::Cmd),
    Chip(chip::Cmd),
    Benchmark(benchmark::Cmd),
}

/// Shared options for core selection, shared between commands
#[derive(clap::Parser)]
pub(crate) struct CoreOptions {
    #[clap(long, default_value = "0")]
    core: usize,
}

/// Determine the default location for the logfile
///
/// This has to be called as early as possible, and while the program
/// is single-threaded. Otherwise, determining the local time might fail.
fn default_logfile_location() -> Result<PathBuf> {
    let project_dirs = directories::ProjectDirs::from("rs", "probe-rs", "probe-rs")
        .context("the application storage directory could not be determined")?;
    let directory = project_dirs.data_dir();
    let logname = sanitize_filename::sanitize_with_options(
        format!(
            "{}.log",
            OffsetDateTime::now_local()?.unix_timestamp_nanos() / 1_000_000
        ),
        sanitize_filename::Options {
            replacement: "_",
            ..Default::default()
        },
    );
    std::fs::create_dir_all(directory).context(format!("{directory:?} could not be created"))?;

    let log_path = directory.join(logname);

    Ok(log_path)
}

fn multicall_check(args: &[OsString], want: &str) -> Option<Vec<OsString>> {
    let argv0 = Path::new(&args[0]);
    if let Some(command) = argv0.file_stem().and_then(|f| f.to_str()) {
        if command == want {
            return Some(args.to_vec());
        }
    }

    if let Some(command) = args.get(1).and_then(|f| f.to_str()) {
        if command == want {
            return Some(args[1..].to_vec());
        }
    }

    None
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if let Some(args) = multicall_check(&args, "cargo-flash") {
        cargo_flash::main(args);
        return Ok(());
    }
    if let Some(args) = multicall_check(&args, "cargo-embed") {
        cargo_embed::main(args);
        return Ok(());
    }

    let utc_offset = UtcOffset::current_local_offset()
        .context("Failed to determine local time for timestamps")?;

    // Parse the commandline options.
    let matches = Cli::parse_from(args);

    // the DAP server has special logging requirements. Run it before initializing logging,
    // so it can do its own special init.
    if let Subcommand::DapServer(cmd) = matches.subcommand {
        return dap_server::run(cmd, utc_offset);
    }

    let log_path = if let Some(location) = matches.log_file {
        location
    } else {
        default_logfile_location().context("Unable to determine default log file location.")?
    };

    let log_file = File::create(&log_path)?;

    let file_subscriber = tracing_subscriber::fmt::layer()
        .json()
        .with_file(true)
        .with_line_number(true)
        .with_span_events(FmtSpan::FULL)
        .with_writer(log_file);

    let stdout_subscriber = tracing_subscriber::fmt::layer()
        .compact()
        .without_time()
        .with_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::ERROR.into())
                .from_env_lossy(),
        );

    tracing_subscriber::registry()
        .with(stdout_subscriber)
        .with(file_subscriber)
        .init();

    tracing::info!("Writing log to {:?}", log_path);

    let result = match matches.subcommand {
        Subcommand::DapServer { .. } => unreachable!(), // handled above.
        Subcommand::List(cmd) => cmd.run(),
        Subcommand::Info(cmd) => cmd.run(),
        Subcommand::Gdb(cmd) => cmd.run(),
        Subcommand::Reset(cmd) => cmd.run(),
        Subcommand::Debug(cmd) => cmd.run(),
        Subcommand::Dump(cmd) => cmd.run(),
        Subcommand::Download(cmd) => cmd.run(),
        Subcommand::Run(cmd) => cmd.run(utc_offset),
        Subcommand::Erase(cmd) => cmd.run(),
        Subcommand::Trace(cmd) => cmd.run(),
        Subcommand::Itm(cmd) => cmd.run(),
        Subcommand::Chip(cmd) => cmd.run(),
        Subcommand::Benchmark(cmd) => cmd.run(),
    };

    tracing::info!("Wrote log to {:?}", log_path);

    result
}
