mod diagnostics;

use colored::*;
use diagnostics::render_diagnostics;
use std::ffi::OsString;
use std::{panic, sync::Mutex};
use std::{path::PathBuf, process, sync::Arc};

use crate::util::common_options::{CargoOptions, FlashOptions, OperationError};
use crate::util::flash;
use clap::{CommandFactory, FromArgMatches};

#[cfg(feature = "sentry")]
use crate::util::logging::{ask_to_log_crash, capture_panic};

use crate::util::{build_artifact, logging, logging::Metadata};

pub fn main(args: Vec<OsString>) {
    let metadata: Arc<Mutex<Metadata>> = Arc::new(Mutex::new(Metadata {
        release: crate::meta::CARGO_VERSION.to_string(),
        commit: crate::meta::GIT_VERSION.to_string(),
        chip: None,
        probe: None,
        speed: None,
    }));

    let metadata_panic = metadata.clone();

    let next = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        #[cfg(feature = "sentry")]
        if ask_to_log_crash() {
            capture_panic(&metadata_panic.lock().unwrap(), info)
        }
        #[cfg(not(feature = "sentry"))]
        log::info!("{:#?}", &metadata_panic.lock().unwrap());
        next(info);
    }));

    #[cfg(not(feature = "sentry"))]
    let metadata_log = metadata.clone();

    match main_try(args, metadata) {
        Ok(_) => (),
        Err(e) => {
            #[cfg(not(feature = "sentry"))]
            log::info!("{:#?}", &metadata_log.lock().unwrap());

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

fn main_try(mut args: Vec<OsString>, metadata: Arc<Mutex<Metadata>>) -> Result<(), OperationError> {
    // When called by Cargo, the first argument after the binary name will be `flash`. If that's the
    // case, remove one argument (`Opt::from_iter` will remove the binary name by itself).
    if args.get(1).and_then(|t| t.to_str()) == Some("flash") {
        args.remove(1);
    }

    // Parse the commandline options with structopt.
    let opt = {
        let matches = FlashOptions::command()
            .bin_name("cargo flash")
            .display_name("cargo-flash")
            .after_help(CargoOptions::help_message("cargo flash"))
            .version(crate::meta::CARGO_VERSION)
            .long_version(crate::meta::LONG_VERSION)
            .get_matches_from(&args);

        FlashOptions::from_arg_matches(&matches)?
    };

    // Initialize the logger with the loglevel given on the commandline.
    logging::init(opt.log);

    // Get the current working dir. Make sure we have a proper default if it cannot be determined.
    let work_dir = opt.work_dir.clone().unwrap_or_else(|| PathBuf::from("."));

    // Load the target description, if given in the cli parameters.
    opt.probe_options.maybe_load_chip_desc()?;

    // Store the chip name in the metadata stuct so we can print it as debug information when cargo-flash crashes.
    if let Some(ref chip) = opt.probe_options.chip {
        metadata.lock().unwrap().chip = Some(format!("{chip:?}"));
    }

    // Change the work dir if the user asked to do so.
    std::env::set_current_dir(&work_dir).map_err(|error| {
        OperationError::FailedToChangeWorkingDirectory {
            source: error,
            path: work_dir.clone(),
        }
    })?;
    log::debug!("Changed working directory to {}", work_dir.display());

    // Get the path to the ELF binary we want to flash.
    // This can either be give from the arguments or can be a cargo build artifact.
    let path: PathBuf = if let Some(path) = &opt.elf {
        path.into()
    } else {
        // Build the project, and extract the path of the built artifact.
        build_artifact(&work_dir, &opt.cargo_options.to_cargo_options())
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

    // Deduce the target to attach to
    let target_selector = opt.probe_options.get_target_selector()?;

    // Attach to specified probe and record metadata
    let probe = opt.probe_options.attach_probe()?;
    {
        let protocol_speed = probe.speed_khz();
        if let Some(speed) = opt.probe_options.speed {
            if protocol_speed < speed {
                log::warn!(
                    "Unable to use specified speed of {} kHz, actual speed used is {} kHz",
                    speed,
                    protocol_speed
                );
            }
        }

        // Store probe speed and name in the metadata struct to be able to
        // print it in case of a crash.
        metadata.lock().unwrap().speed = Some(format!("{protocol_speed:?}"));
        metadata.lock().unwrap().probe = Some(probe.get_name());

        log::info!("Protocol speed {} kHz", protocol_speed);
    }

    // Create a new session
    let mut session = opt.probe_options.attach_session(probe, target_selector)?;

    // Flash the binary
    let flashloader = opt.probe_options.build_flashloader(&mut session, &path)?;
    flash::run_flash_download(&mut session, &path, &opt, flashloader, false)?;

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
