mod cmd;
mod util;

include!(concat!(env!("OUT_DIR"), "/meta.rs"));

use std::path::Path;
use std::str::FromStr;
use std::{ffi::OsString, fs::File, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use probe_rs::flashing::{BinOptions, Format, IdfOptions};
use serde::{de::Error, Deserialize, Deserializer};
use serde_json::Value;
use time::{OffsetDateTime, UtcOffset};
use tracing::metadata::LevelFilter;
use tracing_subscriber::{
    fmt::format::FmtSpan, prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt,
    EnvFilter, Layer,
};

use crate::util::parse_u32;
use crate::util::parse_u64;

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
    DapServer(cmd::dap_server::Cmd),
    /// List all connected debug probes
    List(cmd::list::Cmd),
    /// Gets info about the selected debug probe and connected target
    Info(cmd::info::Cmd),
    /// Resets the target attached to the selected debug probe
    Reset(cmd::reset::Cmd),
    /// Run a GDB server
    Gdb(cmd::gdb::Cmd),
    /// Basic command line debugger
    Debug(cmd::debug::Cmd),
    /// Dump memory from attached target
    Dump(cmd::dump::Cmd),
    /// Download memory to attached target
    Download(cmd::download::Cmd),
    /// Erase all nonvolatile memory of attached target
    Erase(cmd::erase::Cmd),
    /// Flash and run an ELF program
    #[clap(name = "run")]
    Run(cmd::run::Cmd),
    /// Attach to rtt logging
    #[clap(name = "attach")]
    Attach(cmd::attach::Cmd),
    /// Trace a memory location on the target
    #[clap(name = "trace")]
    Trace(cmd::trace::Cmd),
    /// Configure and monitor ITM trace packets from the target.
    #[clap(name = "itm")]
    Itm(cmd::itm::Cmd),
    Chip(cmd::chip::Cmd),
    Benchmark(cmd::benchmark::Cmd),
    Profile(cmd::profile::Cmd),
    /// Read from target memory address
    /// e.g. probe-rs read b32 0x400E1490 2
    ///      Reads 2 32-bit words from address 0x400E1490
    /// Output is a space separated list of hex values padded to the read word width.
    /// e.g. 2 words
    ///     00 00 (8-bit)
    ///     00000000 00000000 (32-bit)
    ///     0000000000000000 0000000000000000 (64-bit)
    ///
    /// NOTE: Only supports RAM addresses
    #[clap(verbatim_doc_comment)]
    Read(cmd::read::Cmd),
    /// Write to target memory address
    /// e.g. probe-rs write b32 0x400E1490 0xDEADBEEF 0xCAFEF00D
    ///      Writes 0xDEADBEEF to address 0x400E1490 and 0xCAFEF00D to address 0x400E1494
    ///
    /// NOTE: Only supports RAM addresses
    #[clap(verbatim_doc_comment)]
    Write(cmd::write::Cmd),
}

/// Shared options for core selection, shared between commands
#[derive(clap::Parser)]
pub(crate) struct CoreOptions {
    #[clap(long, default_value = "0")]
    core: usize,
}

/// A helper function to deserialize a default [`Format`] from a string.
fn format_from_str<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Format, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::String(s) => match Format::from_str(s.as_str()) {
            Ok(format) => Ok(format),
            Err(e) => Err(D::Error::custom(e)),
        },
        _ => Err(D::Error::custom("invalid format")),
    }
}

#[derive(clap::Parser, Clone, Deserialize, Debug, Default)]
#[serde(default)]
pub(crate) struct FormatOptions {
    #[clap(value_enum, ignore_case = true, default_value = "elf", long)]
    #[serde(deserialize_with = "format_from_str")]
    format: Format,
    /// The address in memory where the binary will be put at. This is only considered when `bin` is selected as the format.
    #[clap(long, value_parser = parse_u64)]
    pub base_address: Option<u64>,
    /// The number of bytes to skip at the start of the binary file. This is only considered when `bin` is selected as the format.
    #[clap(long, value_parser = parse_u32, default_value = "0")]
    pub skip: u32,
    /// The idf bootloader path
    #[clap(long)]
    pub idf_bootloader: Option<PathBuf>,
    /// The idf partition table path
    #[clap(long)]
    pub idf_partition_table: Option<PathBuf>,
}

impl FormatOptions {
    pub fn into_format(self) -> anyhow::Result<Format> {
        Ok(match self.format {
            Format::Bin(_) => Format::Bin(BinOptions {
                base_address: self.base_address,
                skip: self.skip,
            }),
            Format::Hex => Format::Hex,
            Format::Elf => Format::Elf,
            Format::Idf(_) => {
                let bootloader = if let Some(path) = self.idf_bootloader {
                    Some(std::fs::read(path)?)
                } else {
                    None
                };

                let partition_table = if let Some(path) = self.idf_partition_table {
                    Some(esp_idf_part::PartitionTable::try_from(std::fs::read(
                        path,
                    )?)?)
                } else {
                    None
                };

                Format::Idf(IdfOptions {
                    bootloader,
                    partition_table,
                })
            }
        })
    }
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

/// Returns the cleaned arguments for the handler of the respective end binary (cli, cargo-flash, cargo-embed, etc.).
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
        cmd::cargo_flash::main(args);
        return Ok(());
    }
    if let Some(args) = multicall_check(&args, "cargo-embed") {
        cmd::cargo_embed::main(args);
        return Ok(());
    }

    let utc_offset = UtcOffset::current_local_offset()
        .context("Failed to determine local time for timestamps")?;

    // Parse the commandline options.
    let matches = Cli::parse_from(args);

    // the DAP server has special logging requirements. Run it before initializing logging,
    // so it can do its own special init.
    if let Subcommand::DapServer(cmd) = matches.subcommand {
        return cmd::dap_server::run(cmd, utc_offset);
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
        Subcommand::Run(cmd) => cmd.run(true, utc_offset),
        Subcommand::Attach(cmd) => cmd.run(utc_offset),
        Subcommand::Erase(cmd) => cmd.run(),
        Subcommand::Trace(cmd) => cmd.run(),
        Subcommand::Itm(cmd) => cmd.run(),
        Subcommand::Chip(cmd) => cmd.run(),
        Subcommand::Benchmark(cmd) => cmd.run(),
        Subcommand::Profile(cmd) => cmd.run(),
        Subcommand::Read(cmd) => cmd.run(),
        Subcommand::Write(cmd) => cmd.run(),
    };

    tracing::info!("Wrote log to {:?}", log_path);

    result
}
