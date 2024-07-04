mod diagnostics;

use clap::Parser;
use colored::Colorize;
use diagnostics::render_diagnostics;
use probe_rs::probe::list::Lister;
use std::ffi::OsString;
use std::{path::PathBuf, process};

use crate::util::cargo::target_instruction_set;
use crate::util::common_options::{
    BinaryDownloadOptions, CargoOptions, OperationError, ProbeOptions,
};
use crate::util::flash;
use crate::util::logging::{setup_logging, LevelFilter};
use crate::util::{cargo::build_artifact, logging};

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
}

pub fn main(args: &[OsString]) {
    match main_try(args) {
        Ok(_) => (),
        Err(e) => {
            // Ensure stderr is flushed before calling process::exit,
            // otherwise the process might panic, because it tries
            // to access stderr during shutdown.
            //
            // We ignore the errors, not much we can do anyway.
            render_diagnostics(e);

            process::exit(1);
        }
    }
}

fn main_try(args: &[OsString]) -> Result<(), OperationError> {
    // Parse the commandline options.
    let opt = CliOptions::parse_from(args);

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

    // Initialize the logger with the loglevel given on the commandline.
    let _log_guard = setup_logging(None, opt.log);

    // Get the path to the binary we want to flash.
    // This can either be give from the arguments or can be a cargo build artifact.
    let image_instr_set;
    let path = if let Some(path_buf) = &opt.path {
        image_instr_set = None;
        path_buf.clone()
    } else {
        let cargo_options = opt.cargo_options.to_cargo_options();
        image_instr_set = target_instruction_set(opt.cargo_options.target.clone());

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

    logging::eprintln(format!(
        "    {} {}",
        "Flashing".green().bold(),
        path.display()
    ));

    let lister = Lister::new();

    // Attach to specified probe
    let (mut session, probe_options) = opt.probe_options.simple_attach(&lister)?;

    // Flash the binary
    let loader =
        flash::build_loader(&mut session, &path, opt.format_options, image_instr_set).unwrap();
    flash::run_flash_download(
        &mut session,
        &path,
        &opt.download_options,
        &probe_options,
        loader,
        false,
    )?;

    // Reset target according to CLI options
    {
        let mut core = session
            .core(0)
            .map_err(OperationError::AttachingToCoreFailed)?;
        if opt.reset_halt {
            core.reset_and_halt(std::time::Duration::from_millis(500))
                .map_err(OperationError::TargetResetHaltFailed)?;
        } else {
            core.reset().map_err(OperationError::TargetResetFailed)?;
        }
    }

    Ok(())
}
