mod config;
mod error;
mod rttui;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use colored::Colorize;
use parking_lot::FairMutex;
use probe_rs::flashing::FormatKind;
use probe_rs::gdb_server::GdbInstanceConfiguration;
use probe_rs::probe::list::Lister;
use probe_rs::rtt::ScanRegion;
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
use crate::util::rtt::client::RttClient;
use crate::util::rtt::{RttChannelConfig, RttConfig};
use crate::util::{cargo::build_artifact, common_options::CargoOptions, logging};
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
    /// Path of a configuration file outside the default path.
    ///
    /// When this is set, the default path is still considered, but the given file is considered
    /// with the highest priority.
    #[arg(long)]
    config_file: Option<String>,
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
    let mut configs = config::Configs::new(work_dir.clone());
    if let Some(config_file) = opt.config_file {
        let config_file = PathBuf::from(config_file);
        if !config_file.exists() {
            // There is a subtle TOC/TOU in here, but this is not a security feature, merely a way
            // to ease debugging for users who mistype their file name.
            return Err(anyhow!("Specified config file does not exist."));
        }
        configs.merge(config_file)?;
    }
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

    let format = FormatOptions::default().to_format_kind(session.target());
    let elf = if matches!(format, FormatKind::Elf | FormatKind::Idf) {
        Some(fs::read(&path)?)
    } else {
        None
    };
    let rtt_client = RttClient::new(
        elf.as_deref(),
        session.target(),
        create_rtt_config(&config).clone(),
        ScanRegion::Ram,
    )?;

    let mut should_clear_rtt_header = true;
    if config.flashing.enabled {
        let download_options = BinaryDownloadOptions {
            disable_progressbars: opt.disable_progressbars,
            disable_double_buffering: config.flashing.disable_double_buffering,
            restore_unwritten: config.flashing.restore_unwritten_bytes,
            flash_layout_output_path: None,
            preverify: config.flashing.preverify,
            verify: config.flashing.verify,
        };
        let format_options = FormatOptions::default();
        let loader = build_loader(&mut session, &path, format_options, image_instr_set)?;

        // When using RTT with a program in flash, the RTT header will be moved to RAM on
        // startup, so clearing it before startup is ok. However, if we're downloading to the
        // header's final address in RAM, then it's not relocated on startup and we should not
        // clear it. This impacts static RTT headers, like used in defmt_rtt.
        if let ScanRegion::Exact(address) = rtt_client.scan_region {
            should_clear_rtt_header = !loader.has_data_for_address(address);
            tracing::debug!("RTT ScanRegion::Exact address is within region to be flashed")
        }

        run_flash_download(
            &mut session,
            &path,
            &download_options,
            &probe_options,
            loader,
            config.flashing.do_chip_erase,
        )?;
    }

    // FIXME: we should probably figure out in a different way which core we can work with.
    // It seems arbitrary that we reset the target using the same core we use for polling RTT.
    let core_id = rtt_client.core_id();

    if config.reset.enabled || config.flashing.enabled {
        let mut core = session.core(core_id)?;
        core.reset_and_halt(Duration::from_millis(500))?;
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
        run_rttui_app(
            name,
            &session,
            config,
            offset,
            should_clear_rtt_header,
            rtt_client,
        )?;
    } else if should_resume_core(&config) {
        // If we don't run the app, we have to resume the core somewhere else.
        let mut session_handle = session.lock();
        let mut core = session_handle.core(0)?;

        if core.core_halted()? {
            core.run()?;
        }
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

fn should_resume_core(config: &config::Config) -> bool {
    if config.flashing.enabled {
        true
    } else {
        !(config.reset.enabled && config.reset.halt_afterwards)
    }
}

fn run_rttui_app(
    name: &str,
    session: &FairMutex<Session>,
    config: config::Config,
    timezone_offset: UtcOffset,
    should_clear_rtt_header: bool,
    mut client: RttClient,
) -> anyhow::Result<()> {
    let core_id = client.core_id();

    if (config.flashing.enabled || config.reset.enabled) && should_clear_rtt_header {
        let mut session_handle = session.lock();
        let mut core = session_handle.core(core_id)?;

        client.clear_control_block(&mut core)?;
    }

    if should_resume_core(&config) {
        let mut session_handle = session.lock();
        let mut core = session_handle.core(core_id)?;

        if core.core_halted()? {
            core.run()?;
        }
    }

    let start = Instant::now();
    let rtt = loop {
        let mut session_handle = session.lock();
        let mut core = session_handle.core(core_id)?;

        if client.try_attach(&mut core)? {
            break client;
        }

        if start.elapsed() > config.rtt.timeout {
            return Err(anyhow!("Failed to attach to RTT: Timeout"));
        }
    };

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

fn create_rtt_config(config: &config::Config) -> RttConfig {
    let mut rtt_config = RttConfig {
        enabled: true,
        channels: vec![],
    };

    // Make sure our defaults are the same as the ones intended in the config struct.
    let default_channel_config = RttChannelConfig::default();

    for channel_config in config.rtt.up_channels.iter() {
        // Where `channel_config` is unspecified, apply default from `default_channel_config`.
        rtt_config.channels.push(RttChannelConfig {
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
        });
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

    rtt_config
}
