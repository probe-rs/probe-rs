mod config;
mod rttui;

use crate::cmd::gdb_server::{GdbInstanceConfiguration, GdbSessionContext};
use anyhow::{Context, Result, anyhow};
use colored::Colorize;
use probe_rs::config::Registry;
use probe_rs::probe::DebugProbeSelector;
use probe_rs::rtt::find_rtt_control_block_in_raw_file;
use probe_rs_rpc::core_ops::WireCoreStatus;
use probe_rs_rpc::flash::BootInfo;
use probe_rs_rpc::format::{FormatKind, FormatOptions};
use probe_rs_rpc::rtt_client::ScanRegion;
use probe_rs_rpc::rtt_config::RttChannelConfig;
use probe_rs_rpc::{Key, RttClient};
use probe_rs_rpc_client::{RpcClient, SessionInterface};
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::time::Instant;
use std::{fs, panic};
use std::{
    path::{Path, PathBuf},
    process,
    time::Duration,
};
use time::{OffsetDateTime, UtcOffset};
use tokio::runtime::Handle;

use crate::util::cargo::cargo_target;
use crate::util::cli;
use crate::util::common_options::{BinaryDownloadOptions, ProbeOptions};
use crate::util::logging::setup_logging;
use crate::util::rtt::RttConfig;
use crate::util::{cargo::build_artifact, common_options::CargoOptions, logging};
use crate::{Config, parse_and_resolve_cli_args, run_app};

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
    config_profile: Option<String>,
    /// Path of a configuration file outside the default path.
    ///
    /// When this is set, the default path is still considered, but the given file is considered
    /// with the highest priority.
    #[arg(long, env = "PROBE_RS_EMBED_CONFIG_FILE")]
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

pub async fn main(args: Vec<OsString>, config: Config, offset: UtcOffset) {
    match main_try(args, config, offset).await {
        Ok(_) => (),
        Err(e) => {
            // Ensure stderr is flushed before calling process::exit,
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

async fn main_try(args: Vec<OsString>, config: Config, offset: UtcOffset) -> Result<()> {
    // Parse the commandline options.
    let opt = parse_and_resolve_cli_args::<CliOptions>(args, &config)?;

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
    let profile_name = opt
        .config_profile
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let mut configs = config::Configs::new(work_dir.clone());
    if let Some(ref config_file) = opt.config_file {
        let config_file = PathBuf::from(config_file);
        if !config_file.exists() {
            // There is a subtle TOC/TOU in here, but this is not a security feature, merely a way
            // to ease debugging for users who mistype their file name.
            return Err(anyhow!("Specified config file does not exist."));
        }
        configs.merge(config_file)?;
    }
    let embed_config = configs.select_defined(&profile_name)?;

    let _log_guard = setup_logging(None, embed_config.general.log_level);

    #[cfg(feature = "remote")]
    let connection_params = embed_config
        .remote
        .host
        .as_ref()
        .map(|host| (host.clone(), embed_config.remote.token.clone()));

    #[cfg(not(feature = "remote"))]
    let connection_params = None;

    run_app(connection_params, async move |client| {
        run_embed(client, opt, embed_config, &profile_name, work_dir, offset).await
    })
    .await
}

async fn run_embed(
    client: RpcClient,
    opt: CliOptions,
    config: config::Config,
    profile_name: &str,
    work_dir: PathBuf,
    offset: UtcOffset,
) -> Result<()> {
    let mut registry = Registry::from_builtin_families();

    // Make sure we load the config given in the cli parameters.
    for cdp in &config.general.chip_descriptions {
        let file = std::fs::read_to_string(Path::new(cdp))?;
        registry
            .add_target_family_from_yaml(&file)
            .with_context(|| format!("failed to load the chip description from {cdp}"))?;
        client
            .load_chip_family(file)
            .await
            .with_context(|| format!("failed to load the chip description from {cdp}"))?;
    }

    let image_instr_set;
    let path = if let Some(path_buf) = &opt.path {
        image_instr_set = None;
        path_buf.clone()
    } else {
        let cargo_options = opt.cargo_options.to_cargo_options();
        image_instr_set = cargo_target(opt.cargo_options.target.as_deref());

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

    logging::println(format!(
        "      {} {}",
        "Profile".green().bold(),
        profile_name
    ));
    logging::println(format!(
        "       {} {}",
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
                interface: config.probe.interface,
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
        protocol: config.probe.protocol,
        non_interactive: !std::io::stdin().is_terminal(),
        probe: selector,
        cycle_power: false,
        speed: config.probe.speed,
        connect_under_reset: config.general.connect_under_reset,
        dry_run: false,
        allow_erase_all: config.flashing.enabled || config.gdb.enabled,
        attach_timeout: None,
    };

    let session = match cli::attach_probe(&client, probe_options, None, false).await {
        Ok(session) => session,
        Err(error) => {
            if let Some(multi) = error.downcast_ref::<cli::MultipleProbesFound>() {
                return Err(anyhow!(
                    "{multi}\n\n\
                    You can also set the [default.probe] config attribute \
                    (in your Embed.toml) to select which probe to use. \
                    For usage examples see https://github.com/probe-rs/probe-rs/blob/master/probe-rs-tools/src/bin/probe-rs/cmd/cargo_embed/config/default.toml ."
                ));
            }
            if let Some(attach) = error.downcast_ref::<cli::TargetAttachFailed>()
                && !attach.connect_under_reset
            {
                tracing::info!("The target seems to be unable to be attached to.");
                tracing::info!(
                    "A hard reset during attaching might help. This will reset the entire chip."
                );
                tracing::info!(
                    "Set `general.connect_under_reset` in your cargo-embed configuration file to enable this feature."
                );
            }
            return Err(error).context("failed attaching to target");
        }
    };

    let target_metadata = session.target_metadata().await?;
    let format_options = FormatOptions::default();
    let format = format_options
        .binary_format
        .resolve_default_format(target_metadata.default_format.as_deref());

    let elf = if matches!(format, FormatKind::Elf | FormatKind::Idf) {
        Some(fs::read(&path)?)
    } else {
        None
    };

    let scan = if let Some(ref elf) = elf {
        if let Ok(Some(addr)) = find_rtt_control_block_in_raw_file(elf) {
            ScanRegion::Exact(addr)
        } else {
            // Do not scan the memory for the control block.
            ScanRegion::Ranges(vec![])
        }
    } else {
        ScanRegion::Ram
    };

    if config.rtt.enabled && matches!(scan, ScanRegion::Ranges(ref ranges) if ranges.is_empty()) {
        return Err(anyhow!(
            "RTT is enabled, but no RTT control block was found in the ELF file"
        ));
    }

    let rtt_config = create_rtt_config(&config);
    let rtt_client = session
        .create_rtt_client(
            scan,
            rtt_config.channels.clone(),
            rtt_config.default_config.clone(),
        )
        .await?;
    let rtt_handle = rtt_client.handle;
    let core_id = rtt_client.core_id as usize;
    let core = session.core(core_id);

    let mut boot_info = BootInfo::Other;
    if config.flashing.enabled {
        let download_options = BinaryDownloadOptions {
            disable_progressbars: opt.disable_progressbars,
            disable_double_buffering: config.flashing.disable_double_buffering,
            restore_unwritten: config.flashing.restore_unwritten_bytes,
            flash_layout_output_path: config.flashing.flash_layout_output_path.clone(),
            preverify: config.flashing.preverify,
            verify: config.flashing.verify,
            chip_erase: config.flashing.do_chip_erase,
            read_flasher_rtt: config.flashing.read_flasher_rtt,
            prefer_flash_algorithm: Vec::new(),
            ram_chunk_size: None,
        };

        boot_info = cli::flash(
            &session,
            &path,
            format_options,
            download_options,
            Some(rtt_handle),
            image_instr_set,
        )
        .await?;
    }

    if config.flashing.enabled || config.reset.enabled {
        prepare_halted_image(
            &session,
            &core,
            &boot_info,
            core_id,
            config.flashing.enabled,
        )
        .await?;
        session.clear_rtt_control_block(rtt_handle).await?;
    }

    let mut gdb_task = None;

    if config.gdb.enabled {
        let gdb_connection_string = config
            .gdb
            .gdb_connection_string
            .clone()
            .unwrap_or_else(|| "127.0.0.1:1337".to_string());

        logging::println(format!(
            "     {} listening at {}",
            "GDB stub".green().bold(),
            gdb_connection_string,
        ));

        let context = GdbSessionContext::from_session(&session, &registry).await?;
        let instances =
            GdbInstanceConfiguration::from_context(&context, Some(gdb_connection_string));
        let session_gdb = session.clone();
        let handle = Handle::current();

        gdb_task = Some(tokio::task::spawn_blocking(move || {
            if let Err(e) =
                crate::cmd::gdb_server::run(session_gdb, handle, context, instances.iter(), None)
            {
                logging::eprintln("During the execution of GDB an error was encountered:");
                logging::eprintln(format!("{e:?}"));
            }
        }));
    }

    if config.rtt.enabled {
        run_rttui_app(name, elf, &session, rtt_handle, core_id, config, offset).await?;
    } else if should_resume_core(&config) {
        let status = core.status().await?;
        if matches!(status, WireCoreStatus::Halted(_)) {
            core.run().await?;
        }
    }

    if let Some(gdb_task) = gdb_task {
        let _ = gdb_task.await;
    }

    logging::println(format!(
        "        {} processing config profile {}",
        "Done".green().bold(),
        profile_name,
    ));

    Ok(())
}

fn should_resume_core(config: &config::Config) -> bool {
    if config.flashing.enabled && !config.reset.halt_afterwards {
        true
    } else {
        !(config.reset.enabled && config.reset.halt_afterwards)
    }
}

/// After flashing or reset, leave the core halted with the image ready to run.
async fn prepare_halted_image(
    session: &SessionInterface,
    core: &probe_rs_rpc_client::CoreInterface,
    boot_info: &BootInfo,
    core_id: usize,
    flashed: bool,
) -> Result<()> {
    if flashed {
        match boot_info {
            BootInfo::FromRam { .. } => {
                session.prepare_boot(boot_info.clone(), core_id).await?;
            }
            BootInfo::Other => {
                core.reset_and_halt(Duration::from_millis(500)).await?;
            }
        }
    } else {
        core.reset_and_halt(Duration::from_millis(500)).await?;
    }

    Ok(())
}

async fn run_rttui_app(
    name: &str,
    elf: Option<Vec<u8>>,
    session: &SessionInterface,
    rtt_handle: Key<RttClient>,
    core_id: usize,
    config: config::Config,
    timezone_offset: UtcOffset,
) -> anyhow::Result<()> {
    let core = session.core(core_id);

    if should_resume_core(&config) {
        let status = core.status().await?;
        if matches!(status, WireCoreStatus::Halted(_)) {
            core.run().await?;
        }
    }

    let start = Instant::now();
    let channels = loop {
        match session.get_rtt_channels(rtt_handle).await {
            Ok(channels) if channels.up.is_empty() && channels.down.is_empty() => {
                if start.elapsed() > config.rtt.timeout {
                    return Err(anyhow!("Failed to attach to RTT: Timeout"));
                }
            }
            Ok(channels) => break channels,
            Err(error) => {
                if start.elapsed() > config.rtt.timeout {
                    return Err(anyhow!("Failed to attach to RTT: {error}"));
                }
            }
        }

        // Throttle attaching. If the target requires stop-mode RTT, this sleep will improve the boot time.
        tokio::time::sleep(Duration::from_millis(10)).await;
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
    let mut app = rttui::app::App::new(
        session.clone(),
        rtt_handle,
        &channels,
        elf,
        config,
        timezone_offset,
        logname,
    )?;
    loop {
        app.render();

        if app.handle_events() {
            logging::println("Shutting down.");
            break;
        }

        app.pump_rtt_io().await?;

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    app.clean_up().await?;

    Ok(())
}

fn create_rtt_config(config: &config::Config) -> RttConfig {
    let mut rtt_config = RttConfig {
        enabled: true,
        channels: vec![],
        default_config: Default::default(),
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

#[cfg(test)]
mod test {
    use super::CliOptions;

    /// clap finds duplicate argument names only in a debug build, and only when it
    /// builds the command. Release builds accept a duplicate and give one of the
    /// two arguments to both fields.
    #[test]
    fn cli_is_valid() {
        use clap::CommandFactory;

        CliOptions::command().debug_assert();
    }
}
