mod config;
mod error;
mod rttui;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use colored::*;
use parking_lot::FairMutex;
use probe_rs::gdb_server::GdbInstanceConfiguration;
use probe_rs::probe::list::Lister;
use probe_rs::rtt::ScanRegion;
use probe_rs::{probe::DebugProbeSelector, Session};
use probe_rs_target::MemoryRegion;
use std::ffi::OsString;
use std::fs;
use std::{
    fs::File,
    io::Write,
    panic,
    path::{Path, PathBuf},
    process,
    sync::Arc,
    time::Duration,
};
use time::{OffsetDateTime, UtcOffset};

use crate::util::cargo::target_instruction_set;
use crate::util::common_options::{BinaryDownloadOptions, OperationError, ProbeOptions};
use crate::util::flash::{build_loader, run_flash_download};
use crate::util::logging::setup_logging;
use crate::util::rtt::{
    self, try_attach_to_rtt_shared, DefmtState, RttActiveTarget, RttChannelConfig, RttConfig,
};
use crate::util::{cargo::build_artifact, common_options::CargoOptions, logging, rtt::DataFormat};
use crate::FormatOptions;

#[derive(Debug, clap::Parser)]
#[clap(
    name = "cargo embed",
    bin_name = "cargo embed",
    version = env!("PROBE_RS_VERSION"),
    long_version = env!("PROBE_RS_LONG_VERSION"),
    after_long_help = CargoOptions::help_message("cargo embed")
)]
struct CliOptions {
    /// Name of the configuration profile to use.
    #[arg()]
    config: Option<String>,
    #[arg(long)]
    chip: Option<String>,
    ///  Use this flag to select a specific probe in the list.
    ///
    ///  Use '--probe VID:PID' or '--probe VID:PID:Serial' if you have more than one probe with the same VID:PID.
    #[arg(long)]
    probe: Option<DebugProbeSelector>,
    #[arg(long)]
    disable_progressbars: bool,
    /// Work directory for the command.
    #[arg(long)]
    work_dir: Option<PathBuf>,
    #[clap(flatten)]
    cargo_options: CargoOptions,
}

pub fn main(args: Vec<OsString>, offset: UtcOffset) {
    match main_try(args, offset) {
        Ok(_) => (),
        Err(e) => {
            // Ensure stderr is flushed before calling proces::exit,
            // otherwise the process might panic, because it tries
            // to access stderr during shutdown.
            //
            // We ignore the errors, not much we can do anyway.

            let mut stderr = std::io::stderr();

            let first_line_prefix = "Error".red().bold();
            let other_line_prefix: String = " ".repeat(first_line_prefix.chars().count());

            let error = format!("{e:?}");

            for (i, line) in error.lines().enumerate() {
                let _ = write!(stderr, "       ");

                if i == 0 {
                    let _ = write!(stderr, "{first_line_prefix}");
                } else {
                    let _ = write!(stderr, "{other_line_prefix}");
                };

                let _ = writeln!(stderr, " {line}");
            }

            let _ = stderr.flush();

            process::exit(1);
        }
    }
}

fn main_try(mut args: Vec<OsString>, offset: UtcOffset) -> Result<()> {
    // When called by Cargo, the first argument after the binary name will be `embed`.
    // If that's the case, remove it.
    if args.get(1).and_then(|t| t.to_str()) == Some("embed") {
        args.remove(1);
    }

    // Parse the commandline options.
    let opt = CliOptions::parse_from(&args);

    // Change the work dir if the user asked to do so.
    if let Some(ref work_dir) = opt.work_dir {
        std::env::set_current_dir(work_dir).with_context(|| {
            format!(
                "Unable to change working directory to {}",
                work_dir.display()
            )
        })?;
    }
    let work_dir = std::env::current_dir()?;

    // Get the config.
    let config_name = opt.config.as_deref().unwrap_or("default");
    let configs = config::Configs::new(work_dir.clone());
    let config = configs.select_defined(config_name)?;

    let _log_guard = setup_logging(None, config.general.log_level);

    // Make sure we load the config given in the cli parameters.
    for cdp in &config.general.chip_descriptions {
        let file = File::open(Path::new(cdp))?;
        probe_rs::config::add_target_from_yaml(file)
            .with_context(|| format!("failed to load the chip description from {cdp}"))?;
    }

    // Remove executable name from the arguments list.
    args.remove(0);

    if let Some(index) = args.iter().position(|x| x == config_name) {
        // We remove the argument we found.
        args.remove(index);
    }

    let cargo_options = opt.cargo_options.to_cargo_options();

    let artifact = build_artifact(&work_dir, &cargo_options)?;

    let path = artifact.path();

    // Get the binary name (without extension) from the build artifact path
    let name = path.file_stem().and_then(|f| f.to_str()).ok_or_else(|| {
        anyhow!(
            "Unable to determine binary file name from path {}",
            path.display()
        )
    })?;

    logging::println(format!("      {} {}", "Config".green().bold(), config_name));
    logging::println(format!(
        "      {} {}",
        "Target".green().bold(),
        path.display()
    ));

    let lister = Lister::new();

    // If we got a probe selector in the config, open the probe matching the selector if possible.
    let selector = if let Some(selector) = opt.probe {
        Some(selector)
    } else {
        match (config.probe.usb_vid.as_ref(), config.probe.usb_pid.as_ref()) {
            (Some(vid), Some(pid)) => Some(DebugProbeSelector {
                vendor_id: u16::from_str_radix(vid, 16)?,
                product_id: u16::from_str_radix(pid, 16)?,
                serial_number: config.probe.serial.clone(),
            }),
            (vid, pid) => {
                if vid.is_some() {
                    tracing::warn!("USB VID ignored, because PID is not specified.");
                }
                if pid.is_some() {
                    tracing::warn!("USB PID ignored, because VID is not specified.");
                }
                None
            }
        }
    };

    let chip = opt
        .chip
        .as_ref()
        .or(config.general.chip.as_ref())
        .map(|chip| chip.into());

    let probe_options = ProbeOptions {
        chip,
        chip_description_path: None,
        protocol: Some(config.probe.protocol),
        probe: selector,
        speed: config.probe.speed,
        connect_under_reset: config.general.connect_under_reset,
        dry_run: false,
        allow_erase_all: config.flashing.enabled || config.gdb.enabled,
    };

    let (mut session, probe_options) = match probe_options.simple_attach(&lister) {
        Ok((session, probe_options)) => (session, probe_options),

        Err(OperationError::MultipleProbesFound { list }) => {
            use std::fmt::Write;

            return Err(anyhow!("The following devices were found:\n \
                    {} \
                        \
                    Use '--probe VID:PID'\n \
                                            \
                    You can also set the [default.probe] config attribute \
                    (in your Embed.toml) to select which probe to use. \
                    For usage examples see https://github.com/probe-rs/cargo-embed/blob/master/src/config/default.toml .",
                    list.iter().enumerate().fold(String::new(), |mut s, (num, link)| { let _ = writeln!(s, "[{num}]: {link}"); s })));
        }
        Err(OperationError::AttachingFailed {
            source,
            connect_under_reset,
        }) => {
            tracing::info!("The target seems to be unable to be attached to.");
            if !connect_under_reset {
                tracing::info!(
                    "A hard reset during attaching might help. This will reset the entire chip."
                );
                tracing::info!("Set `general.connect_under_reset` in your cargo-embed configuration file to enable this feature.");
            }
            return Err(source).context("failed attaching to target");
        }
        Err(e) => return Err(e.into()),
    };

    if config.flashing.enabled {
        // As opposed to cargo-flash, we do not have the option of suppliying an external image.
        // This means we can just take the cargo target, if it's set.
        let image_instr_set = target_instruction_set(opt.cargo_options.target.clone());

        let download_options = BinaryDownloadOptions {
            disable_progressbars: opt.disable_progressbars,
            disable_double_buffering: false,
            restore_unwritten: config.flashing.restore_unwritten_bytes,
            flash_layout_output_path: None,
            verify: false,
        };
        let format_options = FormatOptions::default();
        let loader = build_loader(&mut session, path, format_options, image_instr_set)?;
        run_flash_download(
            &mut session,
            path,
            &download_options,
            &probe_options,
            loader,
            config.flashing.do_chip_erase,
        )?;
    }

    let core_id = rtt::get_target_core_id(&mut session, path);
    if config.reset.enabled {
        let mut core = session.core(core_id)?;
        let halt_timeout = Duration::from_millis(500);
        #[allow(deprecated)] // Remove in 0.10
        if config.flashing.halt_afterwards {
            logging::eprintln(format!(
                "     {} The 'flashing.halt_afterwards' option in the config has moved to the 'reset' section",
                "Warning".yellow().bold()
            ));
            core.reset_and_halt(halt_timeout)?;
        } else if config.reset.halt_afterwards {
            core.reset_and_halt(halt_timeout)?;
        } else {
            core.reset()?;
        }
    }

    let session = Arc::new(FairMutex::new(session));

    let mut gdb_thread_handle = None;

    if config.gdb.enabled {
        let gdb_connection_string = config.gdb.gdb_connection_string.clone();
        let session = session.clone();

        gdb_thread_handle = Some(std::thread::spawn(move || {
            let gdb_connection_string =
                gdb_connection_string.as_deref().unwrap_or("127.0.0.1:1337");

            logging::println(format!(
                "    {} listening at {}",
                "GDB stub".green().bold(),
                gdb_connection_string,
            ));

            let instances = {
                let session = session.lock();
                GdbInstanceConfiguration::from_session(&session, Some(gdb_connection_string))
            };

            if let Err(e) = probe_rs::gdb_server::run(&session, instances.iter()) {
                logging::eprintln("During the execution of GDB an error was encountered:");
                logging::eprintln(format!("{e:?}"));
            }
        }));
    }

    if config.rtt.enabled {
        // GDB is also using the session, so we do not lock on the outside.
        run_rttui_app(name, &session, core_id, config, path, offset)?;
    }

    if let Some(gdb_thread_handle) = gdb_thread_handle {
        let _ = gdb_thread_handle.join();
    }

    logging::println(format!(
        "        {} processing config {}",
        "Done".green().bold(),
        config_name
    ));

    Ok(())
}

fn run_rttui_app(
    name: &str,
    session: &FairMutex<Session>,
    core_id: usize,
    config: config::Config,
    elf_path: &Path,
    timezone_offset: UtcOffset,
) -> anyhow::Result<()> {
    // Transform channel configurations
    let mut rtt_config = RttConfig {
        enabled: true,
        log_format: None,
        channels: vec![],
    };

    // Make sure our defaults are the same as the ones intended in the config struct.
    let default_channel_config = RttChannelConfig::default();

    let mut require_defmt = false;
    for channel_config in config.rtt.up_channels.iter() {
        // Where `channel_config` is unspecified, apply default from `default_channel_config`.
        let rtt_channel_config = RttChannelConfig {
            channel_number: Some(channel_config.channel),
            data_format: channel_config
                .format
                .unwrap_or(default_channel_config.data_format),
            show_timestamps: channel_config
                .show_timestamps
                .unwrap_or(default_channel_config.show_timestamps),
            show_location: channel_config
                .show_location
                .unwrap_or(default_channel_config.show_location),
            defmt_log_format: channel_config
                .defmt_log_format
                .clone()
                .or_else(|| default_channel_config.defmt_log_format.clone()),
            mode: channel_config.mode.or(default_channel_config.mode),
        };
        if rtt_channel_config.data_format == DataFormat::Defmt {
            require_defmt = true;
        }
        rtt_config.channels.push(rtt_channel_config);
    }
    // In case we have down channels without up channels, add them separately.
    for channel_config in config.rtt.down_channels.iter() {
        if config
            .rtt
            .up_channel_config(channel_config.channel)
            .is_none()
        {
            // Set up channel defaults, we don't read from it anyway.
            rtt_config.channels.push(RttChannelConfig {
                channel_number: Some(channel_config.channel),
                data_format: DataFormat::String,
                show_timestamps: false,
                show_location: false,
                defmt_log_format: None,
                mode: None,
            });
        }
    }

    let memory_map = {
        let session_handle = session.lock();
        session_handle.target().memory_map.clone()
    };

    let Some(rtt) = attach_to_rtt_shared(
        session,
        core_id,
        config.rtt.timeout,
        &memory_map,
        &ScanRegion::Ram,
        elf_path,
        &rtt_config,
        timezone_offset,
    )
    .context("Failed to attach to RTT")?
    else {
        tracing::info!("RTT not found, skipping RTT initialization.");
        return Ok(());
    };

    if require_defmt && rtt.defmt_state.is_none() {
        tracing::warn!(
            "RTT channels with format = defmt found, but no defmt metadata found in the ELF file."
        );
    }

    tracing::info!("RTT initialized.");

    // Check if the terminal supports x

    // `App` puts the terminal into a special state, as required
    // by the text-based UI. If a panic happens while the
    // terminal is in that state, this will completely mess up
    // the user's terminal (misformatted panic message, newlines
    // being ignored, input characters not being echoed, ...).
    //
    // The following panic hook cleans up the terminal, while
    // otherwise preserving the behavior of the default panic
    // hook (or whichever custom hook might have been registered
    // before).
    let previous_panic_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        rttui::app::clean_up_terminal();
        previous_panic_hook(panic_info);
    }));

    let chip_name = config.general.chip.as_deref().unwrap_or_default();

    let timestamp_millis = OffsetDateTime::now_utc()
        .to_offset(timezone_offset)
        .unix_timestamp_nanos()
        / 1_000_000;

    let logname = format!("{name}_{chip_name}_{timestamp_millis}");
    let mut app = rttui::app::App::new(rtt, config, logname)?;
    loop {
        app.render();

        {
            let mut session_handle = session.lock();
            let mut core = session_handle.core(core_id)?;

            if app.handle_event(&mut core) {
                logging::println("Shutting down.");
                return Ok(());
            }

            app.poll_rtt(&mut core)?;
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

#[allow(clippy::too_many_arguments)]
fn attach_to_rtt_shared(
    session: &FairMutex<Session>,
    core_id: usize,
    timeout: Duration,
    memory_map: &[MemoryRegion],
    rtt_region: &ScanRegion,
    elf_file: &Path,
    rtt_config: &RttConfig,
    timestamp_offset: UtcOffset,
) -> Result<Option<RttActiveTarget>> {
    // Try to find the RTT control block symbol in the ELF file.
    // If we find it, we can use the exact address to attach to the RTT control block. Otherwise, we
    // fall back to the caller-provided scan regions.
    let elf = fs::read(elf_file)?;
    let scan_region = if let Some(address) = RttActiveTarget::get_rtt_symbol_from_bytes(&elf) {
        ScanRegion::Exact(address as u32)
    } else {
        rtt_region.clone()
    };

    let rtt = try_attach_to_rtt_shared(session, core_id, memory_map, timeout, &scan_region)?;

    let Some(rtt) = rtt else {
        return Ok(None);
    };
    let mut session_handle = session.lock();
    let mut core = session_handle.core(core_id)?;

    let defmt_state = DefmtState::try_from_bytes(&elf)?;
    RttActiveTarget::new(&mut core, rtt, defmt_state, rtt_config, timestamp_offset).map(Some)
}
