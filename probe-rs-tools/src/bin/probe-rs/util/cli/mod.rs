//! CLI-specific building blocks.

use std::sync::Arc;
use std::time::Duration;
use std::{future::Future, path::Path, time::Instant};

use anyhow::Context;
use probe_rs::meta::ElfMetadata;
use probe_rs::rtt::find_rtt_control_block_and_metadata_in_raw_file;
use ratatui::crossterm::style::Stylize;

use crate::rpc::Key;
use crate::rpc::RttClient;
use crate::rpc::functions::probe::convert::{
    from_wire_debug_probe_selector, to_wire_debug_probe_selector, to_wire_protocol,
};
use crate::util::pwr::power_reset;
use crate::util::{
    common_options::{BinaryDownloadOptions, ProbeOptions},
    flash::CliProgressBars,
    logging,
    stream_decoder::DefmtState,
    style::{
        StackTraceAddress, StackTraceFunction, StackTraceInlineMarker, StackTraceSourceLocation,
        probe_rs_color_enabled,
    },
};
use probe_rs_rpc::CancelTopic;
use probe_rs_rpc::flash::{BootInfo, DownloadOptions, FlashLayout, ProgressEvent};
use probe_rs_rpc::format::FormatOptions;
use probe_rs_rpc::probe::{
    AttachRequest, AttachResult, DebugProbeEntry, DebugProbeSelector, SelectProbeResult,
};
use probe_rs_rpc::rtt_client::ScanRegion;
use probe_rs_rpc::stack_trace::StackTrace;
use probe_rs_rpc::stack_trace::StackTraceFrame;
use probe_rs_rpc_client::{HostFiles, RpcClient, SemihostingOptions, SessionInterface};
use probe_rs_zephyr::log::Database as ZephyrLogDatabase;

pub(crate) mod format;
mod monitor;
mod output;
mod test;

pub use monitor::{FirmwareExited, monitor};
pub use output::{DecoderSettings, TargetOutput, rtt_client};
pub use test::test;

pub async fn attach_probe(
    client: &RpcClient,
    mut probe_options: ProbeOptions,
    elf_meta: Option<ElfMetadata>,
    resume_target: bool,
) -> anyhow::Result<SessionInterface> {
    let elf_meta = elf_meta.unwrap_or_default();
    probe_options.chip = select_chip(elf_meta.chip, probe_options.chip)?;

    let request = attach_request(client, probe_options, resume_target).await?;
    let result = with_slow_attach_feedback(client.attach_probe(request)).await?;
    let session = attach_outcome(client, result).await?;

    Ok(SessionInterface::new(client.clone(), session))
}

/// Returns the chip to attach to. Fails if the ELF file and the probe options name different
/// chips.
fn select_chip(
    elf_chip: Option<String>,
    probe_chip: Option<String>,
) -> anyhow::Result<Option<String>> {
    match (elf_chip, probe_chip) {
        (Some(elf_chip), Some(probe_chip))
            if elf_chip.to_lowercase() != probe_chip.to_lowercase() =>
        {
            anyhow::bail!("elf_chip does not match probe_chip")
        }
        (elf_chip, probe_chip) => Ok(probe_chip.or(elf_chip)),
    }
}

/// Returns the success value of `result`, or the failure as an error.
pub async fn attach_outcome<T>(client: &RpcClient, result: AttachResult<T>) -> anyhow::Result<T> {
    match result {
        AttachResult::Success(value) => Ok(value),
        AttachResult::ProbeNotFound => {
            print_setup_hints_if_relevant(client).await;
            Err(ProbeNotFound.into())
        }
        AttachResult::FailedToOpenProbe(error) => {
            print_setup_hints_if_relevant(client).await;
            Err(FailedToOpenProbe(error).into())
        }
        // A busy probe is accessible, so no setup hint here.
        AttachResult::ProbeInUse => Err(ProbeInUse.into()),
        AttachResult::TargetAttachFailed {
            message,
            connect_under_reset,
        } => Err(TargetAttachFailed {
            message,
            connect_under_reset,
        }
        .into()),
    }
}

/// Sends the chip description of `probe_options` to the server, selects the probe and cycles its
/// power if `probe_options` say so.
pub async fn attach_request(
    client: &RpcClient,
    mut probe_options: ProbeOptions,
    resume_target: bool,
) -> anyhow::Result<AttachRequest> {
    // Load the chip description if provided.
    if let Some(chip_description) = probe_options.chip_description_path.take() {
        let file = tokio::fs::read_to_string(&chip_description)
            .await
            .with_context(|| {
                format!(
                    "Failed to read chip description from {}",
                    chip_description.display()
                )
            })?;

        client.load_chip_family(file).await?;
    }

    let mut probe = match select_probe(
        client,
        probe_options.probe.map(to_wire_debug_probe_selector),
        probe_options.non_interactive,
    )
    .await
    {
        Ok(probe) => probe,
        Err(error) => {
            print_setup_hints_if_relevant(client).await;
            return Err(error);
        }
    };

    if probe_options.cycle_power {
        power_reset(
            from_wire_debug_probe_selector(probe.selector()),
            Duration::from_secs(1),
        )
        .await?;
        // The probe gets a new USB device address when it enumerates again.
        if let probe_rs_rpc::probe::ProbeTransport::Usb(usb) = &mut probe.transport {
            usb.usb_location = None;
        }
    }

    Ok(AttachRequest {
        chip: probe_options.chip,
        protocol: probe_options.protocol.map(to_wire_protocol),
        probe,
        speed: probe_options.speed,
        connect_under_reset: probe_options.connect_under_reset,
        allow_erase_all: probe_options.allow_erase_all,
        resume_target,
        wait_for_probe: probe_options.attach_timeout,
        target_serial: probe_options.target_serial,
    })
}

/// How long an attach may run before the user gets a progress indicator.
const ATTACH_FEEDBACK_DELAY: Duration = Duration::from_millis(1500);

/// Displays a spinner while `attach` runs, but only if `attach` is slow.
///
/// An attach that waits for a busy probe can take as long as the configured
/// attach timeout, so without this the CLI looks frozen.
pub async fn with_slow_attach_feedback<F: Future>(attach: F) -> F::Output {
    let mut attach = std::pin::pin!(attach);

    tokio::select! {
        result = &mut attach => return result,
        _ = tokio::time::sleep(ATTACH_FEEDBACK_DELAY) => {}
    }

    let multi_progress = indicatif::MultiProgress::new();
    logging::set_progress_bar(multi_progress.clone());

    let spinner = multi_progress.add(indicatif::ProgressBar::new_spinner());
    spinner.set_style(
        indicatif::ProgressStyle::with_template("{msg:.green.bold} {spinner} {elapsed}")
            .expect("Error in progress bar creation. This is a bug, please report it.")
            .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔"),
    );
    spinner.set_message(format!("{:>13}", "Attaching"));
    spinner.enable_steady_tick(Duration::from_millis(100));

    let result = attach.await;

    spinner.finish_and_clear();
    logging::clear_progress_bar();

    result
}

/// Nudge the user about probe setup after a failed attach over the RPC client.
///
/// Mirrors the direct-attach path: we key off the accessibility reported by
/// listing, not the specific error, so a probe that is present but blocked (or a
/// missing probe) prints the hint, while a busy-but-accessible probe does not.
async fn print_setup_hints_if_relevant(client: &RpcClient) {
    let relevant = match client.list_probes().await {
        Ok(probes) => probes.is_empty() || probes.iter().any(|probe| probe.inaccessible),
        // If we can't even list, something setup-related is plausible.
        Err(_) => true,
    };
    if relevant {
        crate::util::setup_hints::print_setup_hints();
    }
}

pub async fn select_probe(
    client: &RpcClient,
    probe: Option<DebugProbeSelector>,
    non_interactive: bool,
) -> anyhow::Result<DebugProbeEntry> {
    use anyhow::Context as _;
    use std::io::Write as _;

    match client.select_probe(probe).await? {
        SelectProbeResult::Success(probe) => Ok(probe),
        SelectProbeResult::MultipleProbes(list) => {
            if non_interactive {
                return Err(MultipleProbesFound {
                    list: list.iter().map(|probe| probe.to_string()).collect(),
                }
                .into());
            }

            eprintln!("Available Probes:");
            for (i, probe_info) in list.iter().enumerate() {
                eprintln!("{i}: {probe_info}");
            }

            eprint!("Selection: ");
            std::io::stderr().flush().unwrap();

            let mut input = String::new();
            let read = std::io::stdin()
                .read_line(&mut input)
                .context("Failed to read the probe selection")?;

            // End of input: nobody can answer, so fail as `--non-interactive` does.
            if read == 0 {
                eprintln!();
                return Err(MultipleProbesFound {
                    list: list.iter().map(|probe| probe.to_string()).collect(),
                }
                .into());
            }

            let probe_idx = input
                .trim()
                .parse::<usize>()
                .context("Failed to parse probe index")?;

            let probe = list
                .get(probe_idx)
                .ok_or_else(|| anyhow::anyhow!("Probe not found"))?;

            match client.select_probe(Some(probe.selector())).await? {
                SelectProbeResult::Success(probe) => Ok(probe),
                SelectProbeResult::MultipleProbes(_) => {
                    anyhow::bail!("Did not expect multiple probes")
                }
            }
        }
    }
}

/// More than one probe matched, and interactive selection was disabled.
#[derive(Debug)]
pub struct MultipleProbesFound {
    pub list: Vec<String>,
}

impl std::fmt::Display for MultipleProbesFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "The following devices were found:")?;
        for (num, probe) in self.list.iter().enumerate() {
            writeln!(f, "[{num}]: {probe}")?;
        }
        write!(
            f,
            "\nUse '--probe VID:PID' or '--probe VID:PID:Serial' to select one."
        )
    }
}

impl std::error::Error for MultipleProbesFound {}

/// No probe matched the selector / listing.
#[derive(Debug, thiserror::Error)]
#[error("Probe not found")]
pub struct ProbeNotFound;

/// The selected probe is held by another session.
#[derive(Debug, thiserror::Error)]
#[error("Probe is already in use")]
pub struct ProbeInUse;

/// Opening the probe failed before a chip attach was attempted.
#[derive(Debug, thiserror::Error)]
#[error("Failed to open probe: {0}")]
pub struct FailedToOpenProbe(pub String);

/// The probe opened, but connecting to the chip failed.
#[derive(Debug, thiserror::Error)]
#[error("Connecting to the chip was unsuccessful: {message}")]
pub struct TargetAttachFailed {
    pub message: String,
    pub connect_under_reset: bool,
}

pub(crate) fn parse_semihosting_options(
    arg: &[String],
    allow_unverified_server: bool,
) -> anyhow::Result<Option<HostFiles>> {
    let mut options = SemihostingOptions::new();
    for component in arg {
        let parts: Vec<&str> = component.splitn(2, "=").collect();
        match parts[..] {
            // Tolerating empty entries in particular makes a trailing comma tolerated.
            [] => continue,
            [single] => {
                if single.ends_with('/') {
                    options.add_file_prefix(single.into(), single.into())?;
                } else {
                    options.add_file(single.into(), single.into())?;
                }
            }
            [first, second] => {
                if first.starts_with('^') && first.ends_with('$') {
                    options.add_file_regex(first.into(), second.into())?;
                } else if first.ends_with('/') {
                    options.add_file_prefix(first.into(), second.into())?;
                } else {
                    options.add_file(first.into(), second.into())?;
                }
            }
            _ => unreachable!("splitn produces at most 2 items."),
        }
    }
    if options.mappings().is_empty() {
        return Ok(None);
    }
    // Report a bad mapping at the start, and not when the target uses it.
    let host_files = HostFiles::new(&options)?;
    host_files.check_roots()?;
    Ok(Some(
        host_files.allow_unverified_server(allow_unverified_server),
    ))
}

/// Fails if the semihosting mappings are wrong, or the server must not get the mapped files. Call
/// this before the target is attached and flashed.
pub(crate) fn check_semihosting_options(
    client: &RpcClient,
    options: &crate::cmd::run::MonitoringOptions,
) -> anyhow::Result<()> {
    let host_files = parse_semihosting_options(
        &options.semihosting_file,
        options.semihosting_allow_unverified_server,
    )?;
    if let Some(host_files) = host_files {
        client.check_host_files(&host_files)?;
    }
    Ok(())
}

#[derive(Default)]
pub struct FileMetadata {
    pub defmt_data: Option<DefmtState>,
    pub zephyr_log_dictionary: Option<Arc<ZephyrLogDatabase>>,
    pub scan_regions: Option<ScanRegion>,
}

pub async fn parse_metadata(path: &Path) -> anyhow::Result<(FileMetadata, Option<ElfMetadata>)> {
    let elf = tokio::fs::read(path)
        .await
        .with_context(|| format!("Failed to read firmware from {}", path.display()))?;

    let mut elf_meta = None;
    let mut scan_regions = None;
    let mut load_defmt_data = false;

    if let Ok((rtt_block, meta)) = find_rtt_control_block_and_metadata_in_raw_file(&elf) {
        elf_meta = Some(meta);
        match rtt_block {
            Some(addr) => {
                scan_regions = Some(ScanRegion::Exact(addr));
                load_defmt_data = true;
            }
            None => load_defmt_data = !elf.is_empty(),
        }
    }

    let defmt_data = if load_defmt_data {
        DefmtState::try_from_bytes(&elf)?
    } else {
        None
    };

    let zephyr_log_dictionary = match ZephyrLogDatabase::from_elf(&elf) {
        Ok(db) => db.map(Arc::new),
        Err(error) => {
            tracing::warn!("Failed to load the embedded Zephyr log dictionary: {error:#}");
            None
        }
    };

    Ok((
        FileMetadata {
            defmt_data,
            zephyr_log_dictionary,
            scan_regions,
        },
        elf_meta,
    ))
}

pub async fn flash(
    session: &SessionInterface,
    path: &Path,
    format: FormatOptions,
    download_options: BinaryDownloadOptions,
    rtt_client: Option<Key<RttClient>>,
    image_target: Option<String>,
) -> anyhow::Result<BootInfo> {
    // Start timer.
    let flash_timer = Instant::now();

    let mut options = DownloadOptions {
        keep_unwritten_bytes: download_options.restore_unwritten,
        do_chip_erase: download_options.chip_erase,
        skip_erase: false,
        preverify: download_options.preverify,
        verify: download_options.verify,
        disable_double_buffering: download_options.disable_double_buffering,
        preferred_algos: download_options.prefer_flash_algorithm,
        ram_chunk_size: download_options.ram_chunk_size,
    };

    options.sanitize();

    let loader = session
        .build_flash_loader(
            path.to_path_buf(),
            format,
            image_target,
            download_options.read_flasher_rtt,
            rtt_client,
        )
        .await?;

    let mut flash_layout = None;

    let pb = if download_options.disable_progressbars {
        None
    } else {
        Some(CliProgressBars::new())
    };
    session
        .flash(options, loader.loader, async |event| {
            if let ProgressEvent::FlashLayoutReady {
                flash_layout: layout,
            } = &event
            {
                flash_layout = Some(layout.clone());
            }
            if let Some(ref pb) = pb {
                pb.handle(event);
            }
        })
        .await?;

    // Visualise flash layout to file if requested.
    if let Some(visualizer_output) = download_options.flash_layout_output_path
        && let Some(phases) = flash_layout
    {
        let mut flash_layout = FlashLayout::default();
        for phase_layout in phases {
            flash_layout.merge_from(phase_layout);
        }

        let visualizer = crate::util::visualizer::visualize_flash_layout(&flash_layout);
        _ = visualizer.write_svg(visualizer_output);
    }

    logging::eprintln(format!(
        "     {} in {:.02}s",
        "Finished".green().bold(),
        flash_timer.elapsed().as_secs_f32(),
    ));

    Ok(loader.boot_info)
}

async fn display_stack_trace(
    session: &SessionInterface,
    path: &Path,
    stack_frame_limit: u32,
) -> anyhow::Result<()> {
    print!(
        "{}",
        format_stack_trace(session, path, stack_frame_limit).await?
    );

    Ok(())
}

async fn format_stack_trace(
    session: &SessionInterface,
    path: &Path,
    stack_frame_limit: u32,
) -> anyhow::Result<String> {
    use std::fmt::Write as _;

    let stack_trace = session
        .stack_trace(path.to_path_buf(), stack_frame_limit)
        .await?;

    let mut s = String::new();
    for StackTrace { core, frames } in stack_trace.cores.iter() {
        writeln!(s, "Core {core}").unwrap();
        for (i, frame) in frames.iter().enumerate() {
            writeln!(s, "    Frame {i}: {}", format_stack_frame(frame)).unwrap();
        }
        if frames.len() >= stack_frame_limit as usize {
            writeln!(
                s,
                "Use `--stack-frame-limit` to increase the number of frames displayed."
            )
            .unwrap();
        }
    }

    Ok(s)
}

/// Formats a single stack frame for display.
pub(crate) fn format_stack_frame(frame: &StackTraceFrame) -> String {
    use std::fmt::Write as _;

    let color = probe_rs_color_enabled();

    let mut s = String::new();
    write!(
        &mut s,
        "{} @ {}",
        StackTraceFunction::new(frame.function_name.as_str()).colorize(color),
        StackTraceAddress::new(format!("{:#x}", frame.program_counter)).colorize(color),
    )
    .unwrap();
    if frame.is_inlined {
        write!(
            &mut s,
            " {}",
            StackTraceInlineMarker::new("inline").colorize(color)
        )
        .unwrap();
    }
    if let Some(loc) = &frame.location {
        write!(
            &mut s,
            "\n        {}",
            StackTraceSourceLocation::new(format!("{loc}")).colorize(color)
        )
        .unwrap();
    }
    s
}

/// Asks the server to stop the running command. If the connection is gone, the command ends with
/// that error, so a failed send is not an error here.
async fn send_cancel(session: &SessionInterface) {
    if let Err(error) = session.client().publish::<CancelTopic>(&()).await {
        tracing::debug!("Failed to send the cancel: {error}");
    }
}

/// Waits for SIGTERM (Unix) or Ctrl+Break (Windows), as sent by process supervisors.
async fn terminate_signal() -> &'static str {
    cfg_select! {
        unix => {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install signal handler")
                .recv()
                .await;
            "SIGTERM"
        }
        windows => {
            tokio::signal::windows::ctrl_break()
                .expect("failed to install signal handler")
                .recv()
                .await;
            "Ctrl+Break"
        }
        _ => {
            std::future::pending().await
        }
    }
}

/// Runs a future until completion, running another future when a shutdown signal is received.
///
/// This function enables cooperative asynchronous cancellation without dropping the future.
async fn with_ctrl_c<F, I>(f: F, on_ctrl_c: I) -> F::Output
where
    F: Future,
    I: Future,
{
    let mut run = std::pin::pin!(f);

    tokio::select! {
        _ = tokio::signal::ctrl_c() => eprintln!("Received Ctrl+C, exiting"),
        signal = terminate_signal() => eprintln!("Received {signal}, exiting"),
        result = &mut run => return result,
    };

    let (_, r) = tokio::join! {
        on_ctrl_c,
        run,
    };

    r
}
