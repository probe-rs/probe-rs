mod config;
mod error;
mod rttui;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use colored::*;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use probe_rs::gdb_server::GdbInstanceConfiguration;
use probe_rs::rtt::{Rtt, ScanRegion};
use probe_rs::Lister;
use probe_rs::{
    flashing::{download_file_with_options, DownloadOptions, FlashProgress, Format, ProgressEvent},
    DebugProbeSelector, Session,
};
use std::ffi::OsString;
use std::{
    fs,
    fs::File,
    io::Write,
    panic,
    path::{Path, PathBuf},
    process,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use time::{OffsetDateTime, UtcOffset};

use self::rttui::channel::DataFormat;
use crate::util::common_options::{OperationError, ProbeOptions};
use crate::util::logging::setup_logging;
use crate::util::{build_artifact, common_options::CargoOptions, logging};

#[derive(Debug, clap::Parser)]
#[command(after_long_help = CargoOptions::help_message("cargo-embed"))]
struct Opt {
    /// Name of the configuration profile to use.
    #[arg()]
    config: Option<String>,
    #[arg(long)]
    chip: Option<String>,
    ///  Use this flag to select a specific probe in the list.
    ///
    ///  Use '--probe VID:PID' or '--probe VID:PID:Serial' if you have more than one probe with the same VID:PID.
    #[arg(long)]
    probe_selector: Option<DebugProbeSelector>,
    #[arg(long)]
    disable_progressbars: bool,
    /// Work directory for the command.
    #[arg(long)]
    work_dir: Option<PathBuf>,
    #[clap(flatten)]
    cargo_options: CargoOptions,
}

pub fn main(args: Vec<OsString>) {
    // Determine the local offset as early as possible to avoid potential
    // issues with multiple threads and getting the offset.
    let offset = match UtcOffset::current_local_offset() {
        Ok(offset) => offset,
        Err(e) => {
            tracing::debug!("Error getting local offset: {e}");
            tracing::warn!("Unable to determine local time. All timestamps will be in UTC.");
            UtcOffset::UTC
        }
    };

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
    // When called by Cargo, the first argument after the binary name will be `flash`.
    // If that's the case, remove it.
    if args.get(1).and_then(|t| t.to_str()) == Some("embed") {
        args.remove(1);
    }

    // Get commandline options.
    let opt = Opt::parse_from(&args);

    if let Some(work_dir) = opt.work_dir {
        std::env::set_current_dir(&work_dir).with_context(|| {
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
    let selector = if let Some(selector) = opt.probe_selector {
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

    let probe_options = ProbeOptions {
        chip: opt.chip,
        chip_description_path: None,
        protocol: Some(config.probe.protocol),
        probe_selector: selector,
        speed: config.probe.speed,
        connect_under_reset: config.general.connect_under_reset,
        dry_run: false,
        allow_erase_all: config.flashing.enabled || config.gdb.enabled,
    };

    let (mut session, _probe_options) = match probe_options.simple_attach(&lister) {
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
                    list.iter().enumerate().fold(String::new(), |mut s, (num, link)| { let _ = writeln!(s, "[{num}]: {link:?}"); s })));
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
        flash(&config, &mut session, path, opt.disable_progressbars)?;
    }

    if config.reset.enabled {
        let mut core = session.core(0)?;
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

    let session = Arc::new(Mutex::new(session));

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
                let session = session.lock().unwrap();
                GdbInstanceConfiguration::from_session(&session, Some(gdb_connection_string))
            };

            if let Err(e) = probe_rs::gdb_server::run(&session, instances.iter()) {
                logging::eprintln("During the execution of GDB an error was encountered:");
                logging::eprintln(format!("{e:?}"));
            }
        }));
    }

    if config.rtt.enabled {
        let defmt_enable = config
            .rtt
            .channels
            .iter()
            .any(|elem| elem.format == DataFormat::Defmt);

        let defmt_state = if defmt_enable {
            tracing::debug!(
                "Found RTT channels with format = defmt, trying to intialize defmt parsing."
            );
            DefmtInformation::try_read_from_elf(path)?
        } else {
            None
        };

        let rtt_header_address = if let Ok(mut file) = File::open(path) {
            if let Some(address) = rttui::app::App::get_rtt_symbol(&mut file) {
                ScanRegion::Exact(address as u32)
            } else {
                ScanRegion::Ram
            }
        } else {
            ScanRegion::Ram
        };

        let mut rtt = rtt_attach(session.clone(), config.rtt.timeout, &rtt_header_address)
            .context("Failed to attach to RTT")?;

        // Configure rtt channels according to configuration
        rtt_config(session.clone(), &config, &mut rtt)?;

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
            .to_offset(offset)
            .unix_timestamp_nanos()
            / 1_000_000;

        let logname = format!("{name}_{chip_name}_{timestamp_millis}");
        let mut app = rttui::app::App::new(rtt, &config, logname, defmt_state.as_ref())?;
        loop {
            {
                let mut session_handle = session.lock().unwrap();
                let mut core = session_handle.core(0)?;

                app.poll_rtt(&mut core, offset)?;

                app.render();
                if app.handle_event(&mut core) {
                    logging::println("Shutting down.");
                    return Ok(());
                };
            }

            std::thread::sleep(Duration::from_millis(10));
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

fn rtt_config(
    session: Arc<Mutex<Session>>,
    config: &config::Config,
    rtt: &mut Rtt,
) -> Result<(), anyhow::Error> {
    let mut session_handle = session.lock().unwrap();
    let mut core = session_handle.core(0)?;
    let default_up_mode = config.rtt.up_mode;

    for up_channel in rtt.up_channels().iter() {
        let mut specific_mode = None;
        for channel_config in config
            .rtt
            .channels
            .iter()
            .filter(|ch_conf| ch_conf.up == Some(up_channel.number()))
        {
            if let Some(mode) = channel_config.up_mode {
                if specific_mode.is_some() && specific_mode != channel_config.up_mode {
                    // Can't safely resolve this generally...
                    return Err(anyhow!(
                        "Conflicting modes specified for RTT up channel {}: {:?} and {:?}",
                        up_channel.number(),
                        specific_mode.unwrap(),
                        mode
                    ));
                }

                specific_mode = Some(mode);
            }
        }

        if let Some(mode) = specific_mode.or(default_up_mode) {
            // Only set the mode when the config file says to,
            // when not set explicitly, the firmware picks.
            tracing::debug!("Setting RTT channel {} to {:?}", up_channel.number(), &mode);
            up_channel.set_mode(&mut core, mode)?;
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct DefmtInformation {
    table: defmt_decoder::Table,
    /// Location information for defmt
    ///
    /// Optional, defmt decoding is also possible without it.
    location_information: Option<std::collections::BTreeMap<u64, defmt_decoder::Location>>,
}

impl DefmtInformation {
    pub fn try_read_from_elf(path: &Path) -> Result<Option<DefmtInformation>, anyhow::Error> {
        let elf = fs::read(path).with_context(|| {
            format!("Failed to read ELF file from location '{}'", path.display())
        })?;

        let defmt_state = if let Some(table) = defmt_decoder::Table::parse(&elf)? {
            let locs = {
                let locs = table.get_locations(&elf)?;

                if !table.is_empty() && locs.is_empty() {
                    tracing::warn!("Insufficient DWARF info; compile your program with `debug = 2` to enable location info.");
                    None
                } else if table.indices().all(|idx| locs.contains_key(&(idx as u64))) {
                    Some(locs)
                } else {
                    tracing::warn!(
                        "Location info is incomplete; it will be omitted from the output."
                    );
                    None
                }
            };
            Some(DefmtInformation {
                table,
                location_information: locs,
            })
        } else {
            tracing::error!("Defmt enabled in rtt channel config, but defmt table couldn't be loaded from binary.");
            None
        };

        Ok(defmt_state)
    }
}

/// Try to attach to RTT, with the given timeout
fn rtt_attach(
    session: Arc<Mutex<Session>>,
    timeout: Duration,
    rtt_region: &ScanRegion,
) -> Result<Rtt> {
    let t = std::time::Instant::now();

    let mut rtt_init_attempt = 1;

    let mut last_error = None;

    while t.elapsed() < timeout {
        tracing::info!("Initializing RTT (attempt {})...", rtt_init_attempt);
        rtt_init_attempt += 1;

        // Lock the session mutex in a block, so it gets dropped as soon as possible.
        //
        // GDB is also using the session
        {
            let mut session_handle = session.lock().unwrap();
            let memory_map = session_handle.target().memory_map.clone();
            let mut core = session_handle.core(0)?;

            match Rtt::attach_region(&mut core, &memory_map, rtt_region) {
                Ok(rtt) => return Ok(rtt),
                Err(e) => last_error = Some(e),
            }
        }

        tracing::debug!("Failed to initialize RTT. Retrying until timeout.");
        std::thread::sleep(Duration::from_millis(10));
    }

    // Timeout
    if let Some(err) = last_error {
        Err(err.into())
    } else {
        Err(anyhow!("Error setting up RTT"))
    }
}

fn flash(
    config: &config::Config,
    session: &mut probe_rs::Session,
    path: &Path,
    disable_progressbars: bool,
) -> Result<(), anyhow::Error> {
    let instant = Instant::now();
    let mut options = DownloadOptions::new();

    options.keep_unwritten_bytes = config.flashing.restore_unwritten_bytes;
    options.do_chip_erase = config.flashing.do_chip_erase;

    if !disable_progressbars {
        // Create progress bars.
        let multi_progress = MultiProgress::new();
        logging::set_progress_bar(multi_progress.clone());

        let style = ProgressStyle::default_bar()
            .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔")
            .progress_chars("##-")
            .template("{msg:.green.bold} {spinner} [{elapsed_precise}] [{wide_bar}] {bytes:>8}/{total_bytes:>8} @ {bytes_per_sec:>10} (eta {eta:3})")?;

        // Create a new progress bar for the fill progress if filling is enabled.
        let fill_progress = if config.flashing.restore_unwritten_bytes {
            let fill_progress = Arc::new(multi_progress.add(ProgressBar::new(0)));
            fill_progress.set_style(style.clone());
            fill_progress.set_message("     Reading flash  ");
            Some(fill_progress)
        } else {
            None
        };

        // Create a new progress bar for the erase progress.
        let erase_progress = multi_progress.add(ProgressBar::new(0));
        erase_progress.set_style(style.clone());
        erase_progress.set_message("     Erasing sectors");

        // Create a new progress bar for the program progress.
        let program_progress = multi_progress.add(ProgressBar::new(0));
        program_progress.set_style(style);
        program_progress.set_message(" Programming pages  ");

        let flash_layout_output_path = config.flashing.flash_layout_output_path.clone();
        // Register callback to update the progress.
        let progress = FlashProgress::new(move |event| {
            use ProgressEvent::*;
            match event {
                Initialized { flash_layout } => {
                    let total_page_size: u32 = flash_layout.pages().iter().map(|s| s.size()).sum();
                    let total_sector_size: u64 =
                        flash_layout.sectors().iter().map(|s| s.size()).sum();
                    let total_fill_size: u64 = flash_layout.fills().iter().map(|s| s.size()).sum();
                    if let Some(fp) = fill_progress.as_ref() {
                        fp.set_length(total_fill_size)
                    }
                    erase_progress.set_length(total_sector_size);
                    program_progress.set_length(total_page_size as u64);
                    let visualizer = flash_layout.visualize();
                    flash_layout_output_path
                        .as_ref()
                        .map(|path| visualizer.write_svg(path));
                }
                StartedProgramming { length } => {
                    program_progress.enable_steady_tick(Duration::from_millis(100));
                    program_progress.set_length(length);
                    program_progress.reset_elapsed();
                }
                StartedErasing => {
                    erase_progress.enable_steady_tick(Duration::from_millis(100));
                    erase_progress.reset_elapsed();
                }
                StartedFilling => {
                    if let Some(fp) = fill_progress.as_ref() {
                        fp.enable_steady_tick(Duration::from_millis(100))
                    };
                    if let Some(fp) = fill_progress.as_ref() {
                        fp.reset_elapsed()
                    };
                }
                PageProgrammed { size, .. } => {
                    program_progress.inc(size as u64);
                }
                SectorErased { size, .. } => {
                    erase_progress.inc(size);
                }
                PageFilled { size, .. } => {
                    if let Some(fp) = fill_progress.as_ref() {
                        fp.inc(size)
                    };
                }
                FailedErasing => {
                    erase_progress.abandon();
                    program_progress.abandon();
                }
                FinishedErasing => {
                    erase_progress.finish();
                }
                FailedProgramming => {
                    program_progress.abandon();
                }
                FinishedProgramming => {
                    program_progress.finish();
                }
                FailedFilling => {
                    if let Some(fp) = fill_progress.as_ref() {
                        fp.abandon()
                    };
                }
                FinishedFilling => {
                    if let Some(fp) = fill_progress.as_ref() {
                        fp.finish()
                    };
                }
                DiagnosticMessage { .. } => todo!(),
            }
        });

        options.progress = Some(progress);
    }

    download_file_with_options(session, path, Format::Elf, options)
        .with_context(|| format!("failed to flash {}", path.display()))?;

    // If we don't do this, the progress bars disappear.
    logging::clear_progress_bar();

    let elapsed = instant.elapsed();
    logging::println(format!(
        "    {} flashing in {}s",
        "Finished".green().bold(),
        elapsed.as_millis() as f32 / 1000.0,
    ));
    Ok(())
}
