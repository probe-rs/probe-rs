mod cmd;
mod report;
mod rpc;
mod util;

use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::{ffi::OsString, path::PathBuf};

use anyhow::{Context, Result};
use clap::{ArgMatches, CommandFactory, FromArgMatches};
use colored::Colorize;
use figment::Figment;
use figment::providers::{Data, Format as _, Json, Toml, Yaml};
use figment::value::Value;
use itertools::Itertools;
use probe_rs::config::Registry;
use probe_rs::probe::list::Lister;
use report::Report;
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, UtcOffset};

use crate::rpc::functions::RpcApp;
use crate::util::logging::setup_logging;
use probe_rs_rpc_client::{RemoteParams, RpcClient};

const MAX_LOG_FILES: usize = 20;

#[cfg(feature = "remote")]
pub(crate) const HOST_LONG_HELP: &str = "Remote host to connect to.\n\n\
    Accepted URL prefixes:\n  \
    ws://, wss://   a probe-rs serve websocket\n  \
    ssh://          a probe-rs serve websocket, tunnelled through ssh\n  \
    socket://       a probe-rs serve unix socket (Unix only)\n\n\
    The ssh:// form takes [user@]destination[:port] and runs \
    `ssh <destination> -W 127.0.0.1:<port>`, so the server must listen on the \
    loopback interface of the remote host. The port defaults to 3000.\n\n\
    probe-rs gives ssh no other options. Name the destination in your ssh \
    configuration file to select an identity file, a jump host, or an ssh \
    port other than 22.";

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
        help_heading = "REMOTE CONFIGURATION",
        long_help = HOST_LONG_HELP
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
        let mut registry = Registry::from_builtin_families();
        match self.subcommand {
            Subcommand::DapServer(cmd) => {
                let log_path = self.log_file.as_deref();
                cmd::dap_server::run(cmd, Some(client), None, utc_offset, log_path).await
            }
            #[cfg(feature = "remote")]
            Subcommand::Serve(cmd) => cmd.run(_config.server).await,
            Subcommand::List(cmd) => cmd.run(client).await,
            Subcommand::Info(cmd) => cmd.run(client).await,
            Subcommand::Gdb(cmd) => cmd.run(client).await,
            Subcommand::Reset(cmd) => cmd.run(client).await,
            Subcommand::Debug(cmd) => cmd.run(client, utc_offset).await,
            Subcommand::Download(cmd) => cmd.run(client).await,
            Subcommand::Run(cmd) => cmd.run(client, utc_offset).await,
            Subcommand::Attach(cmd) => cmd.run(client, utc_offset).await,
            Subcommand::Verify(cmd) => cmd.run(client).await,
            Subcommand::Erase(cmd) => cmd.run(client).await,
            Subcommand::Trace(cmd) => cmd.run(&mut registry, &lister),
            Subcommand::Itm(cmd) => cmd.run(&mut registry, &lister),
            Subcommand::Chip(cmd) => cmd.run(client).await,
            Subcommand::Benchmark(cmd) => cmd.run(&mut registry, &lister),
            Subcommand::Profile(cmd) => cmd.run(&mut registry, &lister),
            Subcommand::Read(cmd) => cmd.run(client).await,
            Subcommand::Write(cmd) => cmd.run(client).await,
            Subcommand::Complete(cmd) => cmd.run(&lister),
            Subcommand::Mi(cmd) => cmd.run(client).await,
        }
    }

    fn elf(&self) -> Option<PathBuf> {
        match self.subcommand {
            Subcommand::Download(ref cmd) => Some(cmd.path.clone()),
            Subcommand::Run(ref cmd) => Some(cmd.path.clone()),
            Subcommand::Attach(ref cmd) => cmd.path.clone(),
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
    /// Run a GDB server
    Gdb(cmd::gdb_server::Cmd),
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
    fn is_remote_cmd(&self) -> bool {
        // Commands that are implemented via a series of RPC calls.
        // TODO: refactor other commands
        match self {
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
            | Self::Debug(_)
            | Self::DapServer(_) => true,
            Self::Mi(mi) => mi.is_remote_cmd(),
            _ => false,
        }
    }
}

/// Shared options for core selection, shared between commands
#[derive(clap::Parser, Serialize, Deserialize)]
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
fn multicall_check(args: &[OsString], want: &str) -> Option<Vec<OsString>> {
    let argv0 = Path::new(&args[0]);
    if let Some(command) = argv0.file_stem().and_then(|f| f.to_str())
        && command == want
    {
        return Some(args.to_vec());
    }

    if let Some(command) = args.get(1).and_then(|f| f.to_str())
        && command == want
    {
        return Some(args[1..].to_vec());
    }

    None
}

#[tokio::main]
async fn main() -> Result<()> {
    probe_rs_espressif::register_plugin();
    #[cfg(target_os = "linux")]
    probe_rs_linux::register_plugin();

    // Determine the local offset as early as possible to avoid potential
    // issues with multiple threads and getting the offset.
    // FIXME: we should probably let the user know if we can't determine the offset. However,
    //        at this point we don't have a logger yet.
    let utc_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);

    let args: Vec<_> = std::env::args_os().collect();

    let config = load_config().context("Failed to load configuration.")?;

    // Special-case `cargo-embed` and `cargo-flash`.
    if let Some(args) = multicall_check(&args, "cargo-flash") {
        return cmd::cargo_flash::main(args, config).await;
    }
    if let Some(args) = multicall_check(&args, "cargo-embed") {
        cmd::cargo_embed::main(args, config, utc_offset).await;
        return Ok(());
    }

    let mut cli = parse_and_resolve_cli_args::<Cli>(args, &config)?;

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
    let connection_params: RemoteParams = cli
        .host
        .as_ref()
        .map(|host| (host.clone(), cli.token.clone()));

    #[cfg(not(feature = "remote"))]
    let connection_params: RemoteParams = None;

    let is_local = connection_params.is_none();

    let is_tcp_dap = matches!(&cli.subcommand, Subcommand::DapServer(cmd) if cmd.is_tcp_mode());

    let result = if is_tcp_dap {
        let Subcommand::DapServer(cmd) = cli.subcommand else {
            unreachable!("checked DapServer above");
        };
        let log_path = log_path.as_deref();
        cmd::dap_server::run(cmd, None, connection_params, utc_offset, log_path).await
    } else {
        run_app(connection_params, async |client| {
            anyhow::ensure!(
                client.is_local_session() || cli.subcommand.is_remote_cmd(),
                "The subcommand is not supported in remote mode."
            );

            cli.run(client, config, utc_offset).await
        })
        .await
    };

    if is_local {
        // TODO: do something with remote crashes
        compile_report(result, report_path, elf, log_path.as_deref())
    } else {
        result
    }
}

/// Runs the callback using either a local or remote RPC client.
async fn run_app<R>(
    #[cfg_attr(not(feature = "remote"), expect(unused_variables))] connection_params: RemoteParams,
    cb: impl AsyncFnOnce(RpcClient) -> Result<R>,
) -> Result<R> {
    #[cfg(feature = "remote")]
    if let Some((host, token)) = connection_params {
        // Run the command remotely.
        let client = probe_rs_rpc_client::connect(
            &host,
            token.as_deref(),
            &crate::util::meta::rpc_user_agent(),
        )
        .await?;

        return cb(client).await;
    }

    // Create a local server to run commands against.
    let probe_broker = Arc::new(crate::rpc::probe_broker::ProbeBroker::new());
    let (mut local_server, tx, rx) =
        RpcApp::create_server(16, rpc::functions::ProbeAccess::All, probe_broker);
    let handle = tokio::spawn(async move { local_server.run().await });

    // Run the command locally.
    let client = RpcClient::new_local_from_wire(tx, rx);
    let result = cb(client).await;

    // Wait for the server to shut down
    if let Err(e) = handle.await {
        tracing::error!("local RPC server task failed: {e}");
    }

    result
}

fn parse_and_resolve_cli_args<T: FromArgMatches + CommandFactory>(
    mut args: Vec<OsString>,
    config: &Config,
) -> Result<T> {
    // Parse the commandline options.
    let mut matches = T::command().get_matches_from(&args);

    // Apply the configuration preset if one is specified.
    if apply_config_preset::<T>(config, &matches, &mut args)? {
        // Re-parse the modified CLI input.
        matches = T::command().get_matches_from(args);
    }

    Ok(T::from_arg_matches(&matches)?)
}

fn apply_config_preset<T: FromArgMatches + CommandFactory>(
    config: &Config,
    matches: &ArgMatches,
    args: &mut Vec<OsString>,
) -> anyhow::Result<bool> {
    const DEFAULT_PRESET_NAME: &str = "default";

    let preset_name = if let Some(preset) = matches.get_one::<String>("preset") {
        preset.as_str()
    } else if config.presets.contains_key(DEFAULT_PRESET_NAME) {
        DEFAULT_PRESET_NAME
    } else {
        // No --preset in the CLI arguments or environment variables, and no default preset configured.
        return Ok(false);
    };

    let Some(preset) = config.presets.get(preset_name) else {
        anyhow::bail!("Config preset '{preset_name}' not found.");
    };

    let mut args_modified = false;
    for (arg, value) in preset {
        let flag = format!("--{arg}");
        let flag_os = OsString::from(&flag);
        if args.iter().any(|arg| arg == &flag_os) {
            continue;
        }

        if let Value::Bool(_, false) = value {
            continue;
        }

        // Append --flag. For booleans, this is all we do. For strings and
        // numbers, we'll append a value as well.
        args_modified = true;

        let kv = match value {
            Value::String(_, value) => format!("{flag}={value}"),
            Value::Num(_, num) => {
                if let Some(uint) = num.to_u128() {
                    format!("{flag}={uint}")
                } else if let Some(int) = num.to_i128() {
                    format!("{flag}={int}")
                } else if let Some(float) = num.to_f64() {
                    format!("{flag}={float}")
                } else {
                    unreachable!()
                }
            }
            Value::Bool(_, _) => flag.clone(),
            _ => anyhow::bail!("Unsupported value: {value:?}"),
        };
        let kv_os = OsString::from(kv);

        // Filter out any args that are no longer valid after applying the preset.
        //
        // TODO: because we walk one argument at a time, we may reject valid combinations.
        // Unfortunately, even with `ignore_errors(true)`, clap discards unparsed arguments
        // when it finds one that doesn't match, so we can't just pass the unfiltered list
        // to `try_get_matches_from`.
        if T::command()
            .try_get_matches_from(args.iter().chain([&kv_os]))
            .is_ok()
        {
            args.push(kv_os);
        }
    }

    Ok(args_modified)
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
    use crate::Cli;
    use crate::multicall_check;

    /// clap finds duplicate argument names only in a debug build, and only when it
    /// builds the command. Release builds accept a duplicate and give one of the
    /// two arguments to both fields.
    #[test]
    fn cli_is_valid() {
        use clap::CommandFactory;

        Cli::command().debug_assert();
    }

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
