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
use figment::providers::{Data, Format as _, Json, Toml, Yaml};
use figment::Figment;
use itertools::Itertools;
use probe_rs::flashing::{BinOptions, Format, FormatKind, IdfOptions};
use probe_rs::probe::DebugProbeSelector;
use probe_rs::{probe::list::Lister, Target};
use report::Report;
use serde::Deserialize;
use serde::Serialize;
use time::{OffsetDateTime, UtcOffset};

use crate::cmd::run::SharedOptions;
use crate::util::logging::setup_logging;
use crate::util::parse_u32;
use crate::util::parse_u64;

const MAX_LOG_FILES: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceAlias {
    /// The alias of the device.
    pub alias: String,

    /// The probe selector.
    pub selector: DebugProbeSelector,

    /// The chip name.
    pub chip: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// The default device to use.
    pub default_device: Option<String>,

    /// A list of device aliases.
    pub devices: Vec<DeviceAlias>,
}

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
    /// Compare memory to attached target
    Verify(cmd::verify::Cmd),
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

#[derive(clap::Parser, Clone, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct BinaryCliOptions {
    /// The address in memory where the binary will be put at. This is only considered when `bin` is selected as the format.
    #[clap(long, value_parser = parse_u64, help_heading = "DOWNLOAD CONFIGURATION")]
    base_address: Option<u64>,
    /// The number of bytes to skip at the start of the binary file. This is only considered when `bin` is selected as the format.
    #[clap(long, value_parser = parse_u32, default_value = "0", help_heading = "DOWNLOAD CONFIGURATION")]
    skip: u32,
}

#[derive(clap::Parser, Clone, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct IdfCliOptions {
    /// The idf bootloader path
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION")]
    idf_bootloader: Option<PathBuf>,
    /// The idf partition table path
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION")]
    idf_partition_table: Option<PathBuf>,
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
    binary_format: Option<FormatKind>,

    #[clap(flatten)]
    bin_options: BinaryCliOptions,

    #[clap(flatten)]
    idf_options: IdfCliOptions,
}

impl FormatOptions {
    /// If a format is provided, use it.
    /// If a target has a preferred format, we use that.
    /// Finally, if neither of the above cases are true, we default to [`Format::default()`].
    pub fn to_format_kind(&self, target: &Target) -> FormatKind {
        self.binary_format.unwrap_or_else(|| {
            FormatKind::from_optional(target.default_format.as_deref())
                .expect("Failed to parse a default binary format. This shouldn't happen.")
        })
    }

    /// If a format is provided, use it.
    /// If a target has a preferred format, we use that.
    /// Finally, if neither of the above cases are true, we default to [`Format::default()`].
    pub fn into_format(self, target: &Target) -> Format {
        match self.to_format_kind(target) {
            FormatKind::Bin => Format::Bin(BinOptions {
                base_address: self.bin_options.base_address,
                skip: self.bin_options.skip,
            }),
            FormatKind::Hex => Format::Hex,
            FormatKind::Elf => Format::Elf,
            FormatKind::Uf2 => Format::Uf2,
            FormatKind::Idf => Format::Idf(IdfOptions {
                bootloader: self.idf_options.idf_bootloader,
                partition_table: self.idf_options.idf_partition_table,
            }),
        }
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
            if path.extension().is_some_and(|e| e == "log") {
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
                if FormatKind::from_str(format_arg).is_ok() {
                    anyhow::bail!("--format has been renamed to --binary-format. Please use --binary-format {0} instead of --format {0}", format_arg);
                }
            }
        }
    }

    let config = load_config().context("Failed to load configuration.")?;

    // Parse the commandline options.
    let mut matches = Cli::parse_from(args);

    // Substitute options from the global config, before we set up logging
    preprocess_cli_early(&mut matches, &config)?;

    // Setup the probe lister, list all probes normally
    let lister = Lister::new();

    let log_path = if let Some(ref location) = matches.log_file {
        Some(location.clone())
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
    if let Subcommand::DapServer(ref cmd) = matches.subcommand {
        return cmd::dap_server::run(cmd.clone(), &lister, utc_offset, log_path);
    }

    let _logger_guard = setup_logging(log_path, None);

    // Substitute options from the global config after we set up logging
    preprocess_cli_late(&mut matches, &config)?;

    let mut elf = None;
    let result = match matches.subcommand {
        Subcommand::DapServer { .. } => unreachable!(), // handled above.
        Subcommand::List(cmd) => cmd.run(&lister, &config),
        Subcommand::Info(cmd) => cmd.run(&lister),
        Subcommand::Gdb(cmd) => cmd.run(&lister),
        Subcommand::Reset(cmd) => cmd.run(&lister),
        Subcommand::Debug(cmd) => cmd.run(&lister),
        Subcommand::Download(cmd) => {
            elf = Some(cmd.path.clone());
            cmd.run(&lister)
        }
        Subcommand::Run(cmd) => {
            elf = Some(cmd.shared_options.path.clone());
            cmd.run(&lister, true, utc_offset)
        }
        Subcommand::Attach(cmd) => {
            elf = Some(cmd.run.shared_options.path.clone());
            cmd.run(&lister, utc_offset)
        }
        Subcommand::Verify(cmd) => {
            elf = Some(cmd.path.clone());
            cmd.run(&lister)
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

fn load_config() -> anyhow::Result<Config> {
    // Paths to search for the configuration file.
    let mut paths = vec![PathBuf::from(".")];
    if let Some(home) = directories::UserDirs::new().map(|user| user.home_dir().to_path_buf()) {
        paths.push(home);
    }

    // Files to search for, without extension.
    let files = [".probe-rs"];

    let default_config = serde_json::to_string_pretty(&Config::default()).unwrap();
    let mut figment = Figment::from(Data::<Json>::string(&default_config));
    for path in paths {
        for file in files {
            figment = figment
                .merge(Toml::file(path.join(format!("{file}.toml"))))
                .merge(Json::file(path.join(format!("{file}.json"))))
                .merge(Yaml::file(path.join(format!("{file}.yaml"))))
                .merge(Yaml::file(path.join(format!("{file}.yml"))));
        }
    }

    let config = figment.extract::<Config>()?;

    Ok(config)
}

fn preprocess_cli_early(_matches: &mut Cli, _config: &Config) -> Result<()> {
    Ok(())
}

fn preprocess_cli_late(matches: &mut Cli, config: &Config) -> Result<()> {
    resolve_device_aliases(matches, config)?;
    Ok(())
}

fn resolve_device_aliases(matches: &mut Cli, config: &Config) -> Result<()> {
    if let Subcommand::Attach(cmd::attach::Cmd {
        run:
            cmd::run::Cmd {
                shared_options: SharedOptions { probe_options, .. },
                ..
            },
    })
    | Subcommand::Run(cmd::run::Cmd {
        shared_options: SharedOptions { probe_options, .. },
        ..
    })
    | Subcommand::Info(cmd::info::Cmd {
        common: probe_options,
        ..
    }) = &mut matches.subcommand
    {
        // If device is not set, and a selector is not provided, check if there is a default device
        // in the config.
        if probe_options.device.is_none() && probe_options.probe.is_none() {
            if let Some(device) = config.default_device.as_deref() {
                probe_options.device = Some(device.into());
            }
        }

        // If a device alias is set, resolve it to a probe and chip.
        if let Some(alias) = probe_options.device.as_deref() {
            let device = config
                .devices
                .iter()
                .find(|d| d.alias == alias)
                .ok_or_else(|| {
                    anyhow::anyhow!("Device alias {} is not found in the configuration.", alias)
                })?;

            probe_options.probe = Some(device.selector.clone());
            probe_options.chip = device.chip.clone();
        }
    }

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
