mod cmd;
mod report;
mod rpc;
mod util;

use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::{ffi::OsString, path::PathBuf};

use anyhow::{Context, Result};
use clap::{ArgMatches, CommandFactory, FromArgMatches};
use colored::Colorize;
use figment::Figment;
use figment::providers::{Data, Format as _, Json, Toml, Yaml};
use figment::value::Value;
use itertools::Itertools;
use postcard_schema::Schema;
use probe_rs::{Target, probe::list::Lister};
use report::Report;
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, UtcOffset};
use tokio::task::LocalSet;

use crate::rpc::client::RpcClient;
use crate::rpc::functions::RpcApp;
use crate::util::logging::setup_logging;
use crate::util::parse_u32;
use crate::util::parse_u64;

const MAX_LOG_FILES: usize = 20;

type ConfigPreset = HashMap<String, Value>;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct Config {
    #[cfg(feature = "remote")]
    pub server: cmd::serve::ServerConfig,

    /// A named set of `--key=value` pairs.
    pub presets: HashMap<String, ConfigPreset>,
}

#[derive(clap::Parser)]
#[clap(
    name = "probe-rs",
    about = "The probe-rs CLI",
    version = env!("PROBE_RS_VERSION"),
    long_version = env!("PROBE_RS_LONG_VERSION")
)]
struct Cli {
    /// Location for log file for probe-rs's own debug output
    ///
    /// If no location is specified, the behaviour depends on `--log-to-folder`.
    #[clap(long, global = true, help_heading = "DEBUG LOG CONFIGURATION")]
    log_file: Option<PathBuf>,
    /// Enable logging of probe-rs's own debug data to the default folder. This option is ignored if `--log-file` is specified.
    #[clap(long, global = true, help_heading = "DEBUG LOG CONFIGURATION")]
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

    /// Remote host to connect to
    #[cfg(feature = "remote")]
    #[arg(
        long,
        global = true,
        env = "PROBE_RS_REMOTE_HOST",
        help_heading = "REMOTE CONFIGURATION"
    )]
    host: Option<String>,

    /// Authentication token for remote connections
    #[cfg(feature = "remote")]
    #[arg(
        long,
        global = true,
        env = "PROBE_RS_REMOTE_TOKEN",
        help_heading = "REMOTE CONFIGURATION"
    )]
    token: Option<String>,

    #[clap(subcommand)]
    subcommand: Subcommand,

    /// A configuration preset to apply.
    ///
    /// A preset is a list of command line arguments, that can be defined in the configuration file.
    /// Presets can be used as a shortcut to specify any number of options, e.g. they can be used to
    /// assign a name to a specific probe-chip pair.
    ///
    /// Manually specified command line arguments take overwrite presets, but presets
    /// take precedence over environment variables.
    #[arg(long, global = true, env = "PROBE_RS_CONFIG_PRESET")]
    preset: Option<String>,
}

impl Cli {
    async fn run(self, client: RpcClient, _config: Config, utc_offset: UtcOffset) -> Result<()> {
        let lister = Lister::new();
        match self.subcommand {
            Subcommand::DapServer(cmd) => {
                let log_path = self.log_file.as_deref();
                cmd::dap_server::run(cmd, &lister, utc_offset, log_path).await
            }
            #[cfg(feature = "remote")]
            Subcommand::Serve(cmd) => cmd.run(_config.server).await,
            Subcommand::List(cmd) => cmd.run(client).await,
            Subcommand::Info(cmd) => cmd.run(client).await,
            Subcommand::Gdb(cmd) => cmd.run(&mut *client.registry().await, &lister).await,
            Subcommand::Reset(cmd) => cmd.run(client).await,
            Subcommand::Debug(cmd) => {
                cmd.run(&mut *client.registry().await, &lister, utc_offset)
                    .await
            }
            Subcommand::Download(cmd) => cmd.run(client).await,
            Subcommand::Run(cmd) => cmd.run(client, utc_offset).await,
            Subcommand::Attach(cmd) => cmd.run(client, utc_offset).await,
            Subcommand::Verify(cmd) => cmd.run(client).await,
            Subcommand::Erase(cmd) => cmd.run(client).await,
            Subcommand::Trace(cmd) => cmd.run(&mut *client.registry().await, &lister).await,
            Subcommand::Itm(cmd) => cmd.run(&mut *client.registry().await, &lister).await,
            Subcommand::Chip(cmd) => cmd.run(client).await,
            Subcommand::Benchmark(cmd) => cmd.run(&mut *client.registry().await, &lister).await,
            Subcommand::Profile(cmd) => cmd.run(&mut *client.registry().await, &lister).await,
            Subcommand::Read(cmd) => cmd.run(client).await,
            Subcommand::Write(cmd) => cmd.run(client).await,
            Subcommand::Complete(cmd) => cmd.run(&lister).await,
            Subcommand::Mi(cmd) => cmd.run().await,
        }
    }

    fn elf(&self) -> Option<PathBuf> {
        match self.subcommand {
            Subcommand::Download(ref cmd) => Some(cmd.path.clone()),
            Subcommand::Run(ref cmd) => Some(cmd.shared_options.path.clone()),
            Subcommand::Attach(ref cmd) => Some(cmd.run.shared_options.path.clone()),
            Subcommand::Verify(ref cmd) => Some(cmd.path.clone()),
            _ => None,
        }
    }
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
    #[cfg(not(target_arch = "wasm32"))]
    /// Run a GDB server
    Gdb(cmd::gdb_server::Cmd),
    #[cfg(not(target_arch = "wasm32"))]
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
    /// Start a server that accepts remote connections
    #[cfg(feature = "remote")]
    Serve(cmd::serve::Cmd),
    Read(cmd::read::Cmd),
    Write(cmd::write::Cmd),
    Complete(cmd::complete::Cmd),
    Mi(cmd::mi::Cmd),
}

impl Subcommand {
    #[cfg(feature = "remote")]
    fn is_remote_cmd(&self) -> bool {
        // Commands that are implemented via a series of RPC calls.
        // TODO: refactor other commands
        matches!(
            self,
            Self::List(_)
                | Self::Read(_)
                | Self::Write(_)
                | Self::Reset(_)
                | Self::Chip(_)
                | Self::Info(_)
                | Self::Download(_)
                | Self::Attach(_)
                | Self::Run(_)
                | Self::Erase(_)
                | Self::Verify(_)
        )
    }
}

/// Shared options for core selection, shared between commands
#[derive(clap::Parser, Serialize, Deserialize)]
pub(crate) struct CoreOptions {
    #[clap(long, default_value = "0")]
    core: usize,
}

#[derive(clap::Parser, Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[serde(default)]
pub struct BinaryCliOptions {
    /// The address in memory where the binary will be put at. This is only considered when `bin` is selected as the format.
    #[clap(long, value_parser = parse_u64, help_heading = "DOWNLOAD CONFIGURATION")]
    base_address: Option<u64>,
    /// The number of bytes to skip at the start of the binary file. This is only considered when `bin` is selected as the format.
    #[clap(long, value_parser = parse_u32, default_value = "0", help_heading = "DOWNLOAD CONFIGURATION")]
    skip: u32,
}

#[derive(clap::Parser, Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[serde(default)]
pub struct IdfCliOptions {
    /// The idf bootloader path
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION")]
    idf_bootloader: Option<String>,
    /// The idf partition table path
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION")]
    idf_partition_table: Option<String>,
    /// The idf target app partition
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION")]
    idf_target_app_partition: Option<String>,
}

#[derive(clap::Parser, Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[serde(default)]
pub struct ElfCliOptions {
    /// Section name to skip flashing. This option may be specified multiple times, and is only
    /// considered when `elf` is selected as the format.
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION")]
    skip_section: Vec<String>,
}

#[derive(clap::Parser, Clone, Serialize, Deserialize, Debug, Default, Schema)]
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

    #[clap(flatten)]
    elf_options: ElfCliOptions,
}

/// A finite list of all the available binary formats probe-rs understands.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq, Clone, Copy, Schema)]
pub enum FormatKind {
    /// Marks a file in binary format. This means that the file contains the contents of the flash 1:1.
    /// [BinOptions] can be used to define the location in flash where the file contents should be put at.
    /// Additionally using the same config struct, you can skip the first N bytes of the binary file to have them not put into the flash.
    Bin,
    /// Marks a file in [Intel HEX](https://en.wikipedia.org/wiki/Intel_HEX) format.
    Hex,
    /// Marks a file in the [ELF](https://en.wikipedia.org/wiki/Executable_and_Linkable_Format) format.
    #[default]
    Elf,
    /// Marks a file in the [ESP-IDF bootloader](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/api-reference/system/app_image_format.html#app-image-structures) format.
    /// Use [IdfOptions] to configure flashing.
    Idf,
    /// Marks a file in the [UF2](https://github.com/microsoft/uf2) format.
    Uf2,
}

impl FormatKind {
    /// Creates a new Format from an optional string.
    ///
    /// If the string is `None`, the default format is returned.
    pub fn from_optional(s: Option<&str>) -> Result<Self, String> {
        match s {
            Some(format) => Self::from_str(format),
            None => Ok(Self::default()),
        }
    }
}

impl FromStr for FormatKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_lowercase()[..] {
            "bin" | "binary" => Ok(Self::Bin),
            "hex" | "ihex" | "intelhex" => Ok(Self::Hex),
            "elf" => Ok(Self::Elf),
            "uf2" => Ok(Self::Uf2),
            "idf" | "esp-idf" | "espidf" => Ok(Self::Idf),
            _ => Err(format!("Format '{s}' is unknown.")),
        }
    }
}

impl From<FormatKind> for probe_rs::flashing::FormatKind {
    fn from(kind: FormatKind) -> Self {
        match kind {
            FormatKind::Bin => probe_rs::flashing::FormatKind::Bin,
            FormatKind::Hex => probe_rs::flashing::FormatKind::Hex,
            FormatKind::Elf => probe_rs::flashing::FormatKind::Elf,
            FormatKind::Uf2 => probe_rs::flashing::FormatKind::Uf2,
            FormatKind::Idf => probe_rs::flashing::FormatKind::Idf,
        }
    }
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
    fs::create_dir_all(directory)
        .with_context(|| format!("{} could not be created", directory.display()))?;

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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // Determine the local offset as early as possible to avoid potential
    // issues with multiple threads and getting the offset.
    // FIXME: we should probably let the user know if we can't determine the offset. However,
    //        at this point we don't have a logger yet.
    let utc_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);

    let mut args: Vec<_> = std::env::args_os().collect();

    // Special-case `cargo-embed` and `cargo-flash`.
    if let Some(args) = multicall_check(&args, "cargo-flash") {
        cmd::cargo_flash::main(args);
        return Ok(());
    }
    if let Some(args) = multicall_check(&args, "cargo-embed") {
        cmd::cargo_embed::main(args, utc_offset).await;
        return Ok(());
    }

    reject_format_arg(&args)?;

    let config = load_config().context("Failed to load configuration.")?;

    // Parse the commandline options.
    let mut matches = Cli::command().get_matches_from(&args);

    // Apply the configuration preset if one is specified.
    if apply_config_preset(&config, &matches, &mut args)? {
        // Re-parse the modified CLI input. Ignore errors so that users can specify
        // options that are only valid for certain subcommands.
        matches = Cli::command().ignore_errors(true).get_matches_from(args);
    }

    let mut cli = match Cli::from_arg_matches(&matches) {
        Ok(matches) => matches,
        Err(err) => err.exit(),
    };

    // If the user has not specified a log file, we will try to create one in the default location.
    if cli.log_file.is_none() && (cli.log_to_folder || cli.report.is_some()) {
        // We always log if we create a report.
        let location =
            default_logfile_location().context("Unable to determine default log file location.")?;
        prune_logs(
            location
                .parent()
                .expect("A file parent directory. Please report this as a bug."),
        )?;
        cli.log_file = Some(location);
    };
    let log_path = cli.log_file.clone();

    let _logger_guard = if matches!(cli.subcommand, Subcommand::DapServer(_)) {
        // The DAP server has special logging requirements, so skip initializing the logger for it.
        Ok(None)
    } else {
        setup_logging(log_path.as_deref(), None)
    };

    let elf = cli.elf();
    let report_path = cli.report.clone();

    #[cfg(feature = "remote")]
    if let Some(host) = cli.host.as_deref() {
        let host = host.to_string();
        let token = cli.token.clone();
        // Run the command remotely.
        let client =
            tokio::task::spawn_local(async move { rpc::client::connect(&host, token).await })
                .await??;

        anyhow::ensure!(
            cli.subcommand.is_remote_cmd(),
            "The subcommand is not supported in remote mode."
        );

        cli.run(client, config, utc_offset).await?;
        // TODO: handle the report
        return Ok(());
    }

    // Create a local server to run commands against.
    let (mut local_server, tx, rx) = RpcApp::create_server(16, rpc::functions::ProbeAccess::All);
    let handle = tokio::task::spawn_local(async move { local_server.run().await });

    // Run the command locally.
    let client = RpcClient::new_local_from_wire(tx, rx);
    let result = cli.run(client, config, utc_offset).await;

    // Wait for the server to shut down
    handle.await?;

    compile_report(result, report_path, elf, log_path.as_deref())
}

fn apply_config_preset(
    config: &Config,
    matches: &ArgMatches,
    args: &mut Vec<OsString>,
) -> anyhow::Result<bool> {
    let Some(preset) = matches.get_one::<String>("preset") else {
        // No --preset in the CLI arguments or environment variables.
        return Ok(false);
    };

    let Some(preset) = config.presets.get(preset) else {
        anyhow::bail!("Config preset '{preset}' not found.");
    };

    let mut args_modified = false;
    for (arg, value) in preset {
        let flag = format!("--{}", arg).into();
        if args.contains(&flag) {
            continue;
        }

        if let Value::Bool(_, false) = value {
            continue;
        }

        // Append --flag. For booleans, this is all we do. For strings and
        // numbers, we'll append a value as well.
        args_modified = true;
        args.push(flag);

        match value {
            Value::String(_, value) => args.push(value.into()),
            Value::Num(_, num) => {
                if let Some(uint) = num.to_u128() {
                    args.push(format!("{}", uint).into())
                } else if let Some(int) = num.to_i128() {
                    args.push(format!("{}", int).into())
                } else if let Some(float) = num.to_f64() {
                    args.push(format!("{}", float).into())
                } else {
                    unreachable!()
                }
            }
            Value::Bool(_, _) => {}
            _ => anyhow::bail!("Unsupported value: {:?}", value),
        }
    }

    Ok(args_modified)
}

fn reject_format_arg(args: &[OsString]) -> anyhow::Result<()> {
    if let Some(format_arg_pos) = args.iter().position(|arg| arg == "--format") {
        if let Some(format_arg) = args.get(format_arg_pos + 1) {
            if let Some(format_arg) = format_arg.to_str() {
                if FormatKind::from_str(format_arg).is_ok() {
                    anyhow::bail!(
                        "--format has been renamed to --binary-format. Please use --binary-format {0} instead of --format {0}",
                        format_arg
                    );
                }
            }
        }
    }

    Ok(())
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
    // cwd
    let mut paths = vec![PathBuf::from(".")];
    // path to executable
    if let Ok(exe) = std::env::current_exe() {
        paths.push(exe.parent().unwrap().to_path_buf());
    }
    // home directory
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
