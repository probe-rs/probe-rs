mod config;
mod error;
mod rttui;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use colored::Colorize;
use parking_lot::FairMutex;
use probe_rs::gdb_server::GdbInstanceConfiguration;
use probe_rs::probe::list::Lister;
use probe_rs::rtt::{try_attach_to_rtt_shared, Error, ScanRegion};
use probe_rs::{probe::DebugProbeSelector, Session};
use std::ffi::OsString;
use std::time::Instant;
use std::{fs, thread};
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
use crate::util::rtt::{self, DefmtState, RttActiveTarget, RttChannelConfig, RttConfig};
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
    /// The path to the file to be flashed. Setting this will ignore the cargo options.
    #[arg(value_name = "path", long)]
    path: Option<PathBuf>,
    #[clap(flatten)]
    cargo_options: CargoOptions,
}

pub fn main(args: &[OsString], offset: UtcOffset) {
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

fn main_try(args: &[OsString], offset: UtcOffset) -> Result<()> {
    // Parse the commandline options.
    let opt = CliOptions::parse_from(args);

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
    let image_instr_set;
    let path = if let Some(path_buf) = &opt.path {
        image_instr_set = None;
        path_buf.clone()
    } else {
        let cargo_options = opt.cargo_options.to_cargo_options();
        image_instr_set = target_instruction_set(opt.cargo_options.target.clone());

        // Build the project, and extract the path of the built artifact.
        build_artifact(&work_dir, &cargo_options)?.path().into()
    };

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
        non_interactive: false,
        probe: selector,
        speed: config.probe.speed,
        connect_under_reset: config.general.connect_under_reset,
        dry_run: false,
        allow_erase_all: config.flashing.enabled || config.gdb.enabled,
    };

    let lister = Lister::new();
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
                    For usage examples see https://github.com/probe-rs/probe-rs/blob/master/probe-rs-tools/src/bin/probe-rs/cmd/cargo_embed/config/default.toml .",
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
        let download_options = BinaryDownloadOptions {
            disable_progressbars: opt.disable_progressbars,
            disable_double_buffering: config.flashing.disable_double_buffering,
            restore_unwritten: config.flashing.restore_unwritten_bytes,
            flash_layout_output_path: None,
            verify: config.flashing.verify,
        };
        let format_options = FormatOptions::default();
        let loader = build_loader(&mut session, &path, format_options, image_instr_set)?;
        run_flash_download(
            &mut session,
            &path,
            &download_options,
            &probe_options,
            loader,
            config.flashing.do_chip_erase,
        )?;
    }

    let core_id = rtt::get_target_core_id(&mut session, &path);
    if config.reset.enabled {
        let mut core = session.core(core_id)?;
        let halt_timeout = Duration::from_millis(500);
        if config.reset.halt_afterwards {
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

        gdb_thread_handle = Some(thread::spawn(move || {
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
        run_rttui_app(name, &session, core_id, config, &path, offset)?;
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
            log_format: channel_config
                .log_format
                .clone()
                .or_else(|| default_channel_config.log_format.clone()),
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
            .is_some()
        {
            continue;
        }
        // Set up channel defaults, we don't read from it anyway.
        rtt_config.channels.push(RttChannelConfig {
            channel_number: Some(channel_config.channel),
            ..Default::default()
        });
    }

    let Some(rtt) = attach_to_rtt_shared(
        session,
        core_id,
        config.rtt.timeout,
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
                break;
            }

            app.poll_rtt(&mut core)?;
        }

        thread::sleep(Duration::from_millis(10));
    }

    let mut session_handle = session.lock();
    let mut core = session_handle.core(core_id)?;
    app.clean_up(&mut core)?;

    Ok(())
}

fn attach_to_rtt_shared(
    session: &FairMutex<Session>,
    core_id: usize,
    timeout: Duration,
    elf_file: &Path,
    rtt_config: &RttConfig,
    timestamp_offset: UtcOffset,
) -> Result<Option<RttActiveTarget>> {
    // Try to find the RTT control block symbol in the ELF file.
    // If we find it, we can use the exact address to attach to the RTT control block. Otherwise, we
    // fall back to the caller-provided scan regions.
    let elf = fs::read(elf_file)?;
    let scan_region = if let Some(address) = RttActiveTarget::get_rtt_symbol_from_bytes(&elf) {
        ScanRegion::Exact(address)
    } else {
        ScanRegion::Ram
    };

    // FIXME: this is a terrible way to do this. While it works (for a particular error), it's not
    // a general solution and it's also wasting a lot of time on re-parsing ELF and looking for the
    // control block. We should refactor this to only try a single iteration of the attaching
    // once per poll call.
    let start = Instant::now();
    loop {
        let defmt_state = DefmtState::try_from_bytes(&elf)?;
        let rtt = match try_attach_to_rtt_shared(session, core_id, timeout, &scan_region) {
            Ok(rtt) => rtt,
            Err(Error::NoControlBlockLocation) => return Ok(None),
            Err(err) => return Err(anyhow!("Error attempting to attach to RTT: {err}")),
        };

        let mut session_handle = session.lock();
        let mut core = session_handle.core(core_id)?;
        match RttActiveTarget::new(&mut core, rtt, defmt_state, rtt_config, timestamp_offset) {
            Ok(rtta) => return Ok(Some(rtta)),
            Err(error) => {
                if start.elapsed() > timeout {
                    return Err(error);
                }
            }
        }
    }
}
