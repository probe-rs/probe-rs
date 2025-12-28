mod diagnostics;

use anyhow::Context;
use diagnostics::render_diagnostics;
use std::ffi::OsString;
use std::{path::PathBuf, process};

use crate::rpc::client::RpcClient;
use crate::util::cargo::build_artifact;
use crate::util::cargo::cargo_target;
use crate::util::cli;
use crate::util::common_options::{
    BinaryDownloadOptions, CargoOptions, OperationError, ProbeOptions,
};
use crate::util::logging::{LevelFilter, setup_logging};
use crate::{Config, parse_and_resolve_cli_args, run_app};

/// Common options when flashing a target device.
#[derive(Debug, clap::Parser)]
#[clap(
    name = "cargo flash",
    bin_name = "cargo flash",
    version = env!("PROBE_RS_VERSION"),
    long_version = env!("PROBE_RS_LONG_VERSION"),
    after_long_help = CargoOptions::help_message("cargo flash")
)]
struct CliOptions {
    /// Use this flag to reset and halt (instead of just a reset) the attached core after flashing the target.
    #[arg(long)]
    pub reset_halt: bool,
    /// Use this flag to set the log level.
    ///
    /// Configurable via the `RUST_LOG` environment variable.
    /// Default is `warn`. Possible choices are [error, warn, info, debug, trace].
    #[arg(value_name = "level", long)]
    pub log: Option<LevelFilter>,
    /// The path to the file to be flashed. Setting this will ignore the cargo options.
    #[arg(value_name = "path", long)]
    pub path: Option<PathBuf>,
    /// The work directory from which cargo-flash should operate from.
    #[arg(value_name = "directory", long)]
    pub work_dir: Option<PathBuf>,

    #[command(flatten)]
    /// Arguments which are forwarded to 'cargo build'.
    pub cargo_options: CargoOptions,
    #[command(flatten)]
    /// Argument relating to probe/chip selection/configuration.
    pub probe_options: ProbeOptions,
    #[command(flatten)]
    /// Argument relating to probe/chip selection/configuration.
    pub download_options: BinaryDownloadOptions,

    #[command(flatten)]
    pub format_options: crate::FormatOptions,

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

pub async fn main(args: Vec<OsString>, config: Config) -> anyhow::Result<()> {
    // Parse the commandline options.
    let opt = parse_and_resolve_cli_args::<CliOptions>(args, &config)?;

    // Initialize the logger with the loglevel given on the commandline.
    let _log_guard = setup_logging(None, opt.log);

    #[cfg(feature = "remote")]
    let connection_params = opt
        .host
        .as_ref()
        .map(|host| (host.clone(), opt.token.clone()));

    #[cfg(not(feature = "remote"))]
    let connection_params = None;

    let terminate = run_app(connection_params, async |mut client| {
        let main_result = main_try(&mut client, opt).await;

        let r = client.registry().await;

        match main_result {
            Ok(()) => Ok(false),
            Err(e) => {
                // Ensure stderr is flushed before calling process::exit,
                // otherwise the process might panic, because it tries
                // to access stderr during shutdown.
                //
                // We ignore the errors, not much we can do anyway.
                render_diagnostics(&r, e);

                Ok(true)
            }
        }
    })
    .await?;

    if terminate {
        // We've already printed the error with our custom renderer, just exit here.
        process::exit(1);
    }

    Ok(())
}

async fn main_try(client: &mut RpcClient, opt: CliOptions) -> Result<(), OperationError> {
    // Change the work dir if the user asked to do so.
    if let Some(ref work_dir) = opt.work_dir {
        std::env::set_current_dir(work_dir).map_err(|error| {
            OperationError::FailedToChangeWorkingDirectory {
                source: error,
                path: work_dir.clone(),
            }
        })?;
    }
    let work_dir = std::env::current_dir()?;

    // Get the path to the binary we want to flash.
    // This can either be give from the arguments or can be a cargo build artifact.
    let image_instr_set;
    let path = if let Some(path_buf) = &opt.path {
        image_instr_set = None;
        path_buf.clone()
    } else {
        let cargo_options = opt.cargo_options.to_cargo_options();
        image_instr_set = cargo_target(opt.cargo_options.target.as_deref());

        // Build the project, and extract the path of the built artifact.
        build_artifact(&work_dir, &cargo_options)
            .map_err(|error| {
                if let Some(ref work_dir) = opt.work_dir {
                    OperationError::FailedToBuildExternalCargoProject {
                        source: error,
                        // This unwrap is okay, because if we get this error, the path was properly canonicalized on the internal
                        // `cargo build` step.
                        path: work_dir.canonicalize().unwrap(),
                    }
                } else {
                    OperationError::FailedToBuildCargoProject(error)
                }
            })?
            .path()
            .into()
    };

    let session = cli::attach_probe(client, opt.probe_options, false).await?;

    cli::flash(
        &session,
        &path,
        opt.download_options.chip_erase,
        opt.format_options,
        opt.download_options,
        None,
        image_instr_set,
    )
    .await?;

    // Reset target according to CLI options
    let core = session.core(0);

    if opt.reset_halt {
        core.reset_and_halt(std::time::Duration::from_millis(500))
            .await
            .context("Failed to reset and halt target")?;
    } else {
        core.reset().await.context("Failed to reset target")?;
    }

    Ok(())
}
