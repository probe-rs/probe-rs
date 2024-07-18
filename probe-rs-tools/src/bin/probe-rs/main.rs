mod cmd;
mod report;
mod util;

use std::cmp::Reverse;
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::{ffi::OsString, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use colored::Colorize;
use itertools::Itertools;
use probe_rs::flashing::{BinOptions, Format, IdfOptions};
use probe_rs::{probe::list::Lister, Target};
use report::Report;
use serde::Serialize;
use serde::{de::Error, Deserialize, Deserializer};
use serde_json::Value;
use time::{OffsetDateTime, UtcOffset};

use crate::util::logging::setup_logging;
use crate::util::parse_u32;
use crate::util::parse_u64;

const MAX_LOG_FILES: usize = 20;

#[derive(clap::Parser)]
#[clap(
    name = "probe-rs",
    about = "The probe-rs CLI",
    version = env!("PROBE_RS_VERSION"),
    long_version = env!("PROBE_RS_LONG_VERSION")
)]
struct Cli {
    /// Location for log file
    ///
    /// If no location is specified, the behaviour depends on `--log-to-folder`.
    #[clap(long, global = true, help_heading = "LOG CONFIGURATION")]
    log_file: Option<PathBuf>,
    /// Enable logging to the default folder. This option is ignored if `--log-file` is specified.
    #[clap(long, global = true, help_heading = "LOG CONFIGURATION")]
    log_to_folder: bool,
    #[clap(
        long,
        short,
        global = true,
        help_heading = "LOG CONFIGURATION",
        value_name = "PATH",
        require_equals = true,
        num_args = 0..=1,
        default_missing_value = "./report.zip"
    )]
    report: Option<PathBuf>,
    #[clap(subcommand)]
    subcommand: Subcommand,
}

#[derive(clap::Subcommand)]
enum Subcommand {
    /// Debug Adapter Protocol (DAP) server. See <https://probe.rs/docs/tools/debugger/>.
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
    /// Measure the throughput of the selected debug probe
    Benchmark(cmd::benchmark::Cmd),
    /// Profile on-target runtime performance of target ELF program
    Profile(cmd::profile::ProfileCmd),
    Read(cmd::read::Cmd),
    Write(cmd::write::Cmd),
    Complete(cmd::complete::Cmd),
    Mi(cmd::mi::Cmd),
}

/// Shared options for core selection, shared between commands
#[derive(clap::Parser)]
pub(crate) struct CoreOptions {
    #[clap(long, default_value = "0")]
    core: usize,
}

/// A helper function to deserialize a default [`Format`] from a string.
fn format_from_str<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Format>, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::String(s) => match Format::from_str(s.as_str()) {
            Ok(format) => Ok(Some(format)),
            Err(e) => Err(D::Error::custom(e)),
        },
        _ => Ok(None),
    }
}

#[derive(clap::Parser, Clone, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct FormatOptions {
    /// If a format is provided, use it.
    /// If a target has a preferred format, we use that.
    /// Finally, if neither of the above cases are true, we default to ELF.
    #[clap(
        value_enum,
        ignore_case = true,
        long,
        help_heading = "DOWNLOAD CONFIGURATION"
    )]
    // TODO: remove this alias in the next release after 0.24 and release of https://github.com/probe-rs/vscode/pull/86
    #[serde(deserialize_with = "format_from_str", alias = "format")]
    binary_format: Option<Format>,
    /// The address in memory where the binary will be put at. This is only considered when `bin` is selected as the format.
    #[clap(long, value_parser = parse_u64, help_heading = "DOWNLOAD CONFIGURATION")]
    pub base_address: Option<u64>,
    /// The number of bytes to skip at the start of the binary file. This is only considered when `bin` is selected as the format.
    #[clap(long, value_parser = parse_u32, default_value = "0", help_heading = "DOWNLOAD CONFIGURATION")]
    pub skip: u32,
    /// The idf bootloader path
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION")]
    pub idf_bootloader: Option<PathBuf>,
    /// The idf partition table path
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION")]
    pub idf_partition_table: Option<PathBuf>,
}

impl FormatOptions {
    /// If a format is provided, use it.
    /// If a target has a preferred format, we use that.
    /// Finally, if neither of the above cases are true, we default to [`Format::default()`].
    pub fn into_format(self, target: &Target) -> anyhow::Result<Format> {
        let format = self
            .binary_format
            .unwrap_or_else(|| match target.default_format {
                probe_rs_target::BinaryFormat::Idf => Format::Idf(Default::default()),
                probe_rs_target::BinaryFormat::Raw => Default::default(),
            });
        Ok(match format {
            Format::Bin(_) => Format::Bin(BinOptions {
                base_address: self.base_address,
                skip: self.skip,
            }),
            Format::Hex => Format::Hex,
            Format::Elf => Format::Elf,
            Format::Idf(_) => Format::Idf(IdfOptions {
                bootloader: self.idf_bootloader,
                partition_table: self.idf_partition_table,
            }),
            Format::Uf2 => Format::Uf2,
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
    fs::create_dir_all(directory).context(format!("{directory:?} could not be created"))?;

    let log_path = directory.join(logname);

    Ok(log_path)
}

/// Prune all old log files in the `directory`.
fn prune_logs(directory: &Path) -> Result<(), anyhow::Error> {
    // Get the path and elapsed creation time of all files in the log directory that have the '.log'
    // suffix.
    let mut log_files = fs::read_dir(directory)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "log") {
                let metadata = fs::metadata(&path).ok()?;
                let last_modified = metadata.created().ok()?;
                Some((path, last_modified))
            } else {
                None
            }
        })
        .collect_vec();

    // Order all files by the elapsed creation time with smallest first.
    log_files.sort_unstable_by_key(|(_, b)| Reverse(*b));

    // Iterate all files except for the first `MAX_LOG_FILES` and delete them.
    for (path, _) in log_files.iter().skip(MAX_LOG_FILES) {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Returns the cleaned arguments for the handler of the respective end binary
/// (cli, cargo-flash, cargo-embed, etc.)
fn multicall_check<'list>(args: &'list [OsString], want: &str) -> Option<&'list [OsString]> {
    let argv0 = Path::new(&args[0]);
    if let Some(command) = argv0.file_stem().and_then(|f| f.to_str()) {
        if command == want {
            return Some(args);
        }
    }

    if let Some(command) = args.get(1).and_then(|f| f.to_str()) {
        if command == want {
            return Some(&args[1..]);
        }
    }

    None
}

fn main() -> Result<()> {
    // Determine the local offset as early as possible to avoid potential
    // issues with multiple threads and getting the offset.
    // FIXME: we should probably let the user know if we can't determine the offset. However,
    //        at this point we don't have a logger yet.
    let utc_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);

    let args: Vec<_> = std::env::args_os().collect();
    if let Some(args) = multicall_check(&args, "cargo-flash") {
        cmd::cargo_flash::main(args);
        return Ok(());
    }
    if let Some(args) = multicall_check(&args, "cargo-embed") {
        cmd::cargo_embed::main(args, utc_offset);
        return Ok(());
    }

    if let Some(format_arg_pos) = args.iter().position(|arg| arg == "--format") {
        if let Some(format_arg) = args.get(format_arg_pos + 1) {
            if let Some(format_arg) = format_arg.to_str() {
                if Format::from_str(format_arg).is_ok() {
                    anyhow::bail!("--format has been renamed to --binary-format. Please use --binary-format {0} instead of --format {0}", format_arg);
                }
            }
        }
    }

    // Parse the commandline options.
    let matches = Cli::parse_from(args);

    // Setup the probe lister, list all probes normally
    let lister = Lister::new();

    let log_path = if let Some(location) = matches.log_file {
        Some(location)
    } else if matches.log_to_folder || matches.report.is_some() {
        // We always log if we create a report.
        let location =
            default_logfile_location().context("Unable to determine default log file location.")?;
        prune_logs(
            location
                .parent()
                .expect("A file parent directory. Please report this as a bug."),
        )?;
        Some(location)
    } else {
        None
    };
    let log_path = log_path.as_deref();

    // the DAP server has special logging requirements. Run it before initializing logging,
    // so it can do its own special init.
    if let Subcommand::DapServer(cmd) = matches.subcommand {
        return cmd::dap_server::run(cmd, &lister, utc_offset, log_path);
    }

    let _logger_guard = setup_logging(log_path, None);

    let mut elf = None;
    let result = match matches.subcommand {
        Subcommand::DapServer { .. } => unreachable!(), // handled above.
        Subcommand::List(cmd) => cmd.run(&lister),
        Subcommand::Info(cmd) => cmd.run(&lister),
        Subcommand::Gdb(cmd) => cmd.run(&lister),
        Subcommand::Reset(cmd) => cmd.run(&lister),
        Subcommand::Debug(cmd) => cmd.run(&lister),
        Subcommand::Download(cmd) => cmd.run(&lister),
        Subcommand::Run(cmd) => {
            elf = Some(cmd.shared_options.path.clone());
            cmd.run(&lister, true, utc_offset)
        }
        Subcommand::Attach(cmd) => {
            elf = Some(cmd.run.shared_options.path.clone());
            cmd.run(&lister, utc_offset)
        }
        Subcommand::Erase(cmd) => cmd.run(&lister),
        Subcommand::Trace(cmd) => cmd.run(&lister),
        Subcommand::Itm(cmd) => cmd.run(&lister),
        Subcommand::Chip(cmd) => cmd.run(),
        Subcommand::Benchmark(cmd) => cmd.run(&lister),
        Subcommand::Profile(cmd) => cmd.run(&lister),
        Subcommand::Read(cmd) => cmd.run(&lister),
        Subcommand::Write(cmd) => cmd.run(&lister),
        Subcommand::Complete(cmd) => cmd.run(&lister),
        Subcommand::Mi(cmd) => cmd.run(),
    };

    compile_report(result, matches.report, elf, log_path)
}

fn compile_report(
    result: Result<()>,
    path: Option<PathBuf>,
    elf: Option<PathBuf>,
    log_path: Option<&Path>,
) -> Result<()> {
    let Err(error) = result else {
        return Ok(());
    };

    let Some(path) = path else {
        return Err(error);
    };

    let command = std::env::args_os();
    let report = Report::new(command, error, elf.as_deref(), log_path)?;

    report.zip(&path)?;

    eprintln!(
        "{}",
        format!(
            "The compiled report has been written to {}.",
            path.display()
        )
        .blue()
    );
    eprintln!("{}", "Please upload it with your issue on Github.".blue());
    eprintln!(
        "{}",
        "You can create an issue by following this URL:".blue()
    );

    let base = "https://github.com/probe-rs/probe-rs/issues/new";
    let meta = format!("```json\n{}\n```", serde_json::to_string_pretty(&report)?);
    let body = urlencoding::encode(&meta);
    let error = format!("{:#}", report.error);
    let title = urlencoding::encode(&error);

    eprintln!("{base}?labels=bug&title={title}&body={body}");

    Ok(())
}

#[cfg(test)]
mod test {
    use crate::multicall_check;

    #[test]
    fn argument_preprocessing() {
        fn os_strs(args: &[&str]) -> Vec<std::ffi::OsString> {
            args.iter().map(|s| s.into()).collect()
        }

        // cargo embed -h
        assert_eq!(
            multicall_check(&os_strs(&["probe-rs", "cargo-embed", "-h"]), "cargo-embed").unwrap(),
            os_strs(&["cargo-embed", "-h"])
        );

        // cargo flash --chip esp32c2
        assert_eq!(
            multicall_check(
                &os_strs(&["probe-rs", "cargo-flash", "--chip", "esp32c2"]),
                "cargo-flash"
            )
            .unwrap(),
            os_strs(&["cargo-flash", "--chip", "esp32c2"])
        );
    }
}
