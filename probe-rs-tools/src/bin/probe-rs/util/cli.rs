//! CLI-specific building blocks.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::pending;
use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::time::Duration;
use std::{future::Future, path::Path, time::Instant};

use anyhow::Context;
use libtest_mimic::{Completion, Failed, Trial};
use probe_rs::meta::ElfMetadata;
use probe_rs::rtt::find_rtt_control_block_and_metadata_in_raw_file;
use ratatui::crossterm::style::Stylize;
use rustyline_async::{Readline, ReadlineError, ReadlineEvent, SharedWriter};
use time::UtcOffset;
use tokio::io::AsyncWriteExt;
use tokio::runtime::Handle;
use tokio::sync::futures::Notified;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::{Mutex, MutexGuard, Notify};
use tokio_util::sync::CancellationToken;

use crate::cmd::run::{EmbeddedTestElfInfo, MonitoringOptions};
use crate::rpc::Key;
use crate::rpc::RttClient;
use crate::rpc::functions::probe::convert::{
    from_wire_debug_probe_selector, to_wire_debug_probe_selector, to_wire_protocol,
};
use crate::rpc::utils::run_loop::VectorCatchConfig;
use crate::util::pwr::power_reset;
use crate::util::{
    common_options::{BinaryDownloadOptions, ProbeOptions},
    flash::CliProgressBars,
    logging,
    stream_decoder::{AutoDecoder, DefmtProcessor, DefmtState, StreamDecoder, TextDecoder},
    style::{
        Prompt, StackTraceAddress, StackTraceFunction, StackTraceInlineMarker,
        StackTraceSourceLocation, probe_rs_color_enabled, strip_ansi,
    },
};
use probe_rs_rpc::CancelTopic;
use probe_rs_rpc::core_ops::{WireBreakpointCause, WireHaltReason};
use probe_rs_rpc::flash::{BootInfo, DownloadOptions, FlashLayout, ProgressEvent};
use probe_rs_rpc::format::FormatOptions;
use probe_rs_rpc::monitor::{ChannelInfo, MonitorExitReason};
use probe_rs_rpc::monitor::{MonitorMode, MonitorOptions, RttEvent, SemihostingEvent, SerialEvent};
use probe_rs_rpc::probe::{
    AttachRequest, AttachResult, DebugProbeEntry, DebugProbeSelector, SelectProbeResult,
};
use probe_rs_rpc::rtt_client::ScanRegion;
use probe_rs_rpc::rtt_config::RttChannelConfig;
use probe_rs_rpc::semihosting_options::SemihostingOptions;
use probe_rs_rpc::stack_trace::StackTrace;
use probe_rs_rpc::stack_trace::StackTraceFrame;
use probe_rs_rpc::test::{Test, TestResult};
use probe_rs_rpc_client::{ClientError, MonitorEvent, RpcClient, SessionInterface};
use probe_rs_zephyr::log::{Database as ZephyrLogDatabase, Decoder as ZephyrLogDecoder};

type TargetOutputFiles = std::collections::HashMap<ChannelIdentifier, tokio::fs::File>;

pub async fn attach_probe(
    client: &RpcClient,
    mut probe_options: ProbeOptions,
    elf_meta: Option<ElfMetadata>,
    resume_target: bool,
) -> anyhow::Result<SessionInterface> {
    let elf_meta = elf_meta.unwrap_or_default();

    if let Some(elf_chip) = &elf_meta.chip
        && let Some(probe_chip) = &probe_options.chip
        && elf_chip.to_lowercase() != probe_chip.to_lowercase()
    {
        anyhow::bail!("elf_chip does not match probe_chip");
    }
    probe_options.chip = probe_options.chip.or(elf_meta.chip);

    let request = attach_request(client, probe_options, resume_target).await?;
    let result = with_slow_attach_feedback(client.attach_probe(request)).await?;
    let session = attach_outcome(client, result).await?;

    Ok(SessionInterface::new(client.clone(), session))
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
        probe.usb_location = None;
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
            std::io::stdin()
                .read_line(&mut input)
                .expect("Expect input for probe selection");

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

/// A selector for a named stream, be it an RTT or a semihosting channel.
///
/// When converting from text (eg. as a CLI argument), the `Unqualified` variant is only produced
/// when there is no colon in the name; otherwise, the prefix before the colon is matched into a
/// variant.
///
/// ```
/// assert_eq!(ChannelIdentifier::Unqualified("foo".to_string()), "foo".parse().unwrap());
/// assert_eq!(ChannelIdentifier::Rtt("defmt".to_string()), "rtt:defmt".parse().unwrap());
/// assert_eq!(ChannelIdentifier::CatchAll, "".parse().unwrap());
/// ```
// Could we be smart with the Strings and implement this for any type and then do some AsRef and
// the right tricks to access a hashmap keyed with an owned identifier with a borrowed one? Maybe.
// But allocators are fast, this won't be a bottleneck, and it is easy to maintain with
// always-owned channels.
#[derive(PartialEq, Eq, Hash, Clone)]
pub(crate) enum ChannelIdentifier {
    /// A named channel (might match a semihosting or an RTT channel)
    Unqualified(String),
    /// A named RTT channel
    Rtt(String),
    /// A named semihosting channel
    Semihosting(String),
    /// Selector that matches any channel; depending on the context, this usually means "any
    /// channel that is not explicitly handled".
    CatchAll,
}

impl std::str::FromStr for ChannelIdentifier {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        Ok(match s.splitn(2, ':').collect::<Vec<_>>().as_slice() {
            [] => unreachable!(),
            [""] => ChannelIdentifier::CatchAll,
            [unqualified] => ChannelIdentifier::Unqualified(unqualified.to_string()),
            ["rtt", rtt] => ChannelIdentifier::Rtt(rtt.to_string()),
            ["semihosting", semihosting] => ChannelIdentifier::Semihosting(semihosting.to_string()),
            _ => anyhow::bail!(
                "Channel identifiers with colons need to be qualified as `rtt:name` or `semihosting:name`."
            ),
        })
    }
}

impl ChannelIdentifier {
    /// Returns an `Unqualified(name)` for any qualified name.
    fn unqualified(&self) -> Option<Self> {
        match self {
            ChannelIdentifier::Rtt(n) => Some(ChannelIdentifier::Unqualified(n.clone())),
            ChannelIdentifier::Semihosting(n) => Some(ChannelIdentifier::Unqualified(n.clone())),
            _ => None,
        }
    }

    /// Picks a channel out of a map of channels, falling back to using an unqualified version of
    /// the same value, or the map's catch-all channel.
    fn find_in<'res, T>(
        &self,
        map: &'res mut std::collections::HashMap<Self, T>,
    ) -> Option<&'res mut T> {
        // This double/triple access (get / get_mut) is a bit weird, but the compiler will not see
        // that the lifetimes of the get_mut are non-overlapping if we return the Ok of an initial
        // get_mut.
        if map.contains_key(self) {
            return map.get_mut(self);
        };
        if let Some(fallback) = self.unqualified()
            && map.contains_key(&fallback)
        {
            return map.get_mut(&fallback);
        };
        map.get_mut(&Self::CatchAll)
    }
}

/// Splits argument text strings like `['channel1=file-for-c1', 'stdout=some-file', 'defaultfile']` by the
/// `=` signs, mapping the keys to a [`ChannelIdentifier`] (or
/// [`CatchAll`][ChannelIdentifier::CatchAll] when no key present) and opening the values as files
/// in append mode.
pub(crate) async fn connect_target_output_files(
    arg: &[String],
) -> anyhow::Result<TargetOutputFiles> {
    let mut map = TargetOutputFiles::new();
    for component in arg {
        let parts: Vec<&str> = component.splitn(2, "=").collect();
        let (key, value) = match parts[..] {
            // Tolerating empty entries in particular makes a trailing comma tolerated.
            [] => continue,
            [single] => (ChannelIdentifier::CatchAll, single),
            [first, second] => (first.parse()?, second),
            _ => unreachable!("splitn produces at most 2 items."),
        };
        let value = tokio::fs::OpenOptions::new()
            .read(false)
            .append(true)
            .create(true)
            .open(value)
            .await?;
        map.insert(key, value);
    }
    Ok(map)
}

pub(crate) fn parse_semihosting_options(arg: &[String]) -> anyhow::Result<SemihostingOptions> {
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
    Ok(options)
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

pub async fn rtt_client(
    session: &SessionInterface,
    meta: &FileMetadata,
    monitor_options: &MonitoringOptions,
) -> anyhow::Result<CliRttClient> {
    let scan_regions = match &meta.scan_regions {
        Some(scan_regions) => scan_regions.clone(),
        None => monitor_options.scan_region.clone(),
    };

    let zephyr_log_dictionary = match &monitor_options.zephyr_log_dictionary {
        Some(path) => {
            let data = tokio::fs::read(path).await.with_context(|| {
                format!(
                    "Failed to read Zephyr log dictionary from {}",
                    path.display()
                )
            })?;
            Some(Arc::new(ZephyrLogDatabase::from_bytes(&data)?))
        }
        None => meta.zephyr_log_dictionary.clone(),
    };

    // We don't really know what to configure here, so we set a default configuration if we can, but that's it.
    let rtt_client = session
        .create_rtt_client(
            scan_regions,
            vec![],
            RttChannelConfig {
                mode: Some(monitor_options.rtt_channel_mode),
                ..Default::default()
            },
        )
        .await?;

    // The actual data processor objects will be created once we have the channel names.
    Ok(CliRttClient {
        handle: rtt_client.handle,
        channel_processors: vec![],
        zephyr_log_dictionary,
    })
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

// Monitor starts in read-only mode: it outputs logs, but has no prompt to type into.
// When channels are discovered, it can either stay in read-only mode, or switch to interactive mode if down channels are available.
// Interactive mode allows the user to type into the prompt, and send data to the target.

struct MonitorUiContext {
    change_notifier: Notify,
    ui_state: Mutex<MonitorUiState>,
}

impl MonitorUiContext {
    pub fn new(selected_down_channel: u32) -> Self {
        let change_notifier = Notify::new();
        let ui_state = Mutex::new(MonitorUiState {
            exited: false,
            rtt_client: None,
            up_channels: Vec::new(),
            down_channels: Vec::new(),
            selected_down_channel,
            serial_input: false,
            shared_writer: None,
        });
        Self {
            change_notifier,
            ui_state,
        }
    }

    async fn exit(&self) {
        self.ui_state.lock().await.exit();
        self.change_notifier.notify_waiters();
    }

    async fn update(&self, with: impl FnOnce(&mut MonitorUiState)) {
        let mut ui_state = self.ui_state.lock().await;
        with(&mut ui_state);
        self.change_notifier.notify_waiters();
    }

    fn subscribe(&self) -> Notified<'_> {
        self.change_notifier.notified()
    }

    fn lock(&self) -> impl Future<Output = MutexGuard<'_, MonitorUiState>> {
        self.ui_state.lock()
    }
}

#[derive(Clone)]
struct MonitorUiState {
    exited: bool,
    rtt_client: Option<Key<RttClient>>,
    up_channels: Vec<ChannelInfo>,
    down_channels: Vec<ChannelInfo>,
    selected_down_channel: u32,
    /// The target serial port sent data, so it can receive input too.
    serial_input: bool,
    shared_writer: Option<SharedWriter>,
}
/// A target of the user input.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InputTarget {
    Rtt(u32),
    Serial,
}

impl MonitorUiState {
    /// The input targets: the RTT down channels, then the serial port.
    fn input_targets(&self) -> Vec<(InputTarget, String)> {
        let mut targets = self
            .down_channels
            .iter()
            .enumerate()
            .map(|(i, channel)| (InputTarget::Rtt(i as u32), channel.name.clone()))
            .collect::<Vec<_>>();
        if self.serial_input {
            targets.push((InputTarget::Serial, "serial".to_string()));
        }
        targets
    }

    fn print(&mut self, message: &str) {
        if let Some(writer) = self.shared_writer.as_mut() {
            _ = writer.write_all(message.as_bytes());
        } else {
            print!("{message}");
        }
    }

    fn exit(&mut self) {
        self.exited = true;
        self.shared_writer = None;
    }
}

pub async fn monitor(
    session: &SessionInterface,
    mode: MonitorMode,
    path: Option<&Path>,
    monitor_options: &MonitoringOptions,
    mut output: TargetOutput,
    vector_catch: VectorCatchConfig,
) -> anyhow::Result<()> {
    let semihosting_options = parse_semihosting_options(&monitor_options.semihosting_file)?;
    let mut target_output_files =
        connect_target_output_files(&monitor_options.target_output_file).await?;

    let options = MonitorOptions {
        catch_reset: vector_catch.catch_reset,
        catch_hardfault: vector_catch.catch_hardfault,
        catch_svc: vector_catch.catch_svc,
        catch_hlt: vector_catch.catch_hlt,
        rtt_client: output.rtt_handle(),
        semihosting_options,
    };

    // The mutex around the context should only be held for a short period of time.
    let ui_context = MonitorUiContext::new(monitor_options.rtt_down_channel);

    let monitor = session.monitor(mode, options, async |msg| {
        if let MonitorEvent::Rtt(RttEvent::Discovered {
            down_channels,
            up_channels,
        }) = &msg
        {
            ui_context
                .update(|state| {
                    state.up_channels = up_channels.clone();
                    state.down_channels = down_channels.clone();
                    state.rtt_client = output.rtt_handle();
                })
                .await
        };
        if matches!(msg, MonitorEvent::Serial(_)) && !ui_context.lock().await.serial_input {
            ui_context.update(|state| state.serial_input = true).await;
        }

        handle_monitor_event(
            &mut output,
            msg,
            &mut target_output_files,
            &async |message| ui_context.lock().await.print(message),
            &async |message| eprint!("{message}"),
            &monitor_options.rtt_up_channels,
        )
        .await;
    });

    // SIGTERM (Unix) / Ctrl+Break (Windows) handler
    let terminate = async {
        let signal = terminate_signal().await;
        eprintln!("Received {signal}, exiting");
        send_cancel(session).await;
        pending().await
    };

    fn prompt_possible() -> bool {
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
    }

    // Gets activated when the RTT client discovers down channels, or the target serial port
    // sends data. Displays a prompt and waits for user input. The input targets can change while
    // the prompt runs, so the prompt reads them again for each event.
    async fn cli_with_prompt(session: &SessionInterface, context: &MonitorUiContext) {
        let (targets, selected_down_channel) = {
            let state = context.lock().await;
            (state.input_targets(), state.selected_down_channel)
        };
        let mut selected = targets
            .iter()
            .map(|(target, _)| *target)
            .find(|target| *target == InputTarget::Rtt(selected_down_channel))
            .unwrap_or(targets[0].0);

        let prompt = |targets: &[(InputTarget, String)], selected: InputTarget| {
            let name = targets
                .iter()
                .find(|(target, _)| *target == selected)
                .map_or("", |(_, name)| name.as_str());
            Prompt::new(format!("{name}> ")).to_string()
        };

        let Ok((mut rl, sw)) = Readline::new(prompt(&targets, selected)) else {
            eprintln!("Failed to create readline");
            _ = tokio::signal::ctrl_c().await;

            eprintln!("Received Ctrl+C, exiting");
            return;
        };

        let _prompt_logs = logging::install_prompt_writer(sw.clone());

        context
            .update(|data| data.shared_writer = Some(sw.clone()))
            .await;

        rl.should_print_line_on(true, false);
        loop {
            match rl.readline().await {
                Ok(ReadlineEvent::Line(mut line)) => {
                    rl.add_history_entry(line.clone());
                    line.push('\n');
                    match selected {
                        InputTarget::Rtt(channel) => {
                            let rtt_client = context.lock().await.rtt_client;
                            if let Some(client) = rtt_client
                                && let Err(error) = session
                                    .send_to_rtt(client, channel, line.into_bytes(), 0)
                                    .await
                            {
                                eprintln!("Error sending data to RTT: {:?}", error);
                                break;
                            }
                        }
                        InputTarget::Serial => {
                            let len = line.len();
                            // The firmware may not read its input. Report it and continue.
                            match session.send_to_serial(line.into_bytes()).await {
                                Ok(written) if written as usize == len => {}
                                Ok(written) => {
                                    eprintln!("The serial port took {written} of {len} bytes")
                                }
                                Err(error) => {
                                    eprintln!("Error sending data to the serial port: {error:?}")
                                }
                            }
                        }
                    }
                }
                Ok(ReadlineEvent::Eof) => {
                    // Select the next input target.
                    let targets = context.lock().await.input_targets();
                    let next = targets
                        .iter()
                        .position(|(target, _)| *target == selected)
                        .map_or(0, |i| (i + 1) % targets.len());
                    selected = targets[next].0;
                    if let Err(error) = rl.update_prompt(&prompt(&targets, selected)) {
                        eprintln!("Error updating prompt: {:?}", error);
                        break;
                    }
                }
                Ok(ReadlineEvent::Interrupted) => {
                    eprintln!("Received Ctrl+C, exiting");
                    break;
                }
                Err(ReadlineError::Closed) => break,
                Err(ReadlineError::IO(err)) => {
                    eprintln!("IO error: {}", err);
                    break;
                }
            }
        }

        context.exit().await;

        _ = rl.flush();
    }

    // Main UI loop. Detects changes generated either by the user or received from
    // the server, and decides what to display based on the current state.
    let ui = async {
        const LIST_RTT_TIMEOUT: Duration = Duration::from_secs(5);
        let list_rtt_deadline = Instant::now() + LIST_RTT_TIMEOUT;

        loop {
            enum DisplayMode {
                OutputOnly,
                CliWithPrompt,
                ListChannelsAndQuit,
                Exited,
            }

            let state = {
                let locked = ui_context.lock().await;

                if locked.exited {
                    DisplayMode::Exited
                } else if monitor_options.list_rtt {
                    DisplayMode::ListChannelsAndQuit
                } else if locked.input_targets().is_empty() || !prompt_possible() {
                    DisplayMode::OutputOnly
                } else {
                    DisplayMode::CliWithPrompt
                }
            };
            match state {
                DisplayMode::OutputOnly => {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
                            eprintln!("Received Ctrl+C, exiting");
                            ui_context.exit().await;
                        },
                        _ = ui_context.subscribe() => {}
                    }
                }
                DisplayMode::CliWithPrompt => cli_with_prompt(session, &ui_context).await,
                DisplayMode::ListChannelsAndQuit => {
                    // Subscribe before the state check so that a discovery
                    // notification is not lost between the check and the wait.
                    let notified = ui_context.subscribe();
                    let mut data = ui_context.lock().await;
                    if data.rtt_client.is_some() {
                        fn print_channels(channels: &[ChannelInfo]) {
                            if channels.is_empty() {
                                println!("  None.");
                                return;
                            }
                            for (i, channel) in channels.iter().enumerate() {
                                println!("  {}: {}", i, ChannelInfoPrinter(channel));
                            }
                        }
                        println!("Up channels:");
                        print_channels(&data.up_channels);
                        println!("Down channels:");
                        print_channels(&data.down_channels);
                        data.exit();
                    } else {
                        drop(data);
                        tokio::select! {
                            _ = tokio::signal::ctrl_c() => {
                                eprintln!("Received Ctrl+C, exiting");
                                ui_context.exit().await;
                            }
                            _ = tokio::time::sleep(list_rtt_deadline.saturating_duration_since(Instant::now())) => {
                                eprintln!("Failed to attach to RTT: Timeout");
                                ui_context.exit().await;
                            }
                            _ = notified => {}
                        }
                    }
                }
                DisplayMode::Exited => break,
            }
        }
        send_cancel(session).await;
        pending().await
    };

    // We exit when one of the futures cancels the session and the monitor exits.
    // TODO: this should be a loop
    let result = tokio::select! {
        result = monitor => result,

        // These futures are never supposed to resolve. They shall trigger
        // a cancellation event, then the monitor future will handle the rest.
        _ = ui => unreachable!(),
        _ = terminate => unreachable!(),
    };

    let (print_stack_trace, result) = match result {
        Ok(MonitorExitReason::SemihostingExit(Ok(_))) => {
            // On success, we only print if the user asked for it.
            (
                monitor_options.always_print_stacktrace,
                Ok("Firmware exited successfully"),
            )
        }
        Ok(MonitorExitReason::UserExit) => {
            // On ctrl-c, we only print if the user asked for it.
            (
                monitor_options.always_print_stacktrace,
                Ok("Exited by user request"),
            )
        }
        Ok(MonitorExitReason::Halted(halt_reason)) => {
            let reason = describe_halt_reason(halt_reason).to_string();
            (true, Err(FirmwareExited::Unexpected(reason).into()))
        }
        Ok(MonitorExitReason::SemihostingExit(Err(details))) => {
            let reason = match details.reason {
                // HW vector reason codes
                0x20000 => String::from("Branch through zero"),
                0x20001 => String::from("Undefined instruction"),
                0x20002 => String::from("Software interrupt"),
                0x20003 => String::from("Prefetch abort"),
                0x20004 => String::from("Data abort"),
                0x20005 => String::from("Address exception"),
                0x20006 => String::from("IRQ"),
                0x20007 => String::from("FIQ"),
                // SW reason codes
                0x20020 => String::from("Breakpoint"),
                0x20021 => String::from("Watchpoint"),
                0x20022 => String::from("Step complete"),
                0x20023 => String::from("Unknown runtime error"),
                0x20024 => String::from("Internal error"),
                0x20025 => String::from("User interruption"),
                0x20026 => String::from("Application exit"),
                0x20027 => String::from("Stack overflow"),
                0x20028 => String::from("Division by zero"),
                0x20029 => String::from("OS specific error"),
                other => format!("Unknown exit reason {other}"),
            };

            let subcode = match details.reason {
                0x20026 => match details.subcode {
                    Some(134) => String::from(" (Aborted)"),
                    Some(other) => format!(" (Unknown exit code {other})"),
                    None => String::from(""),
                },
                _ => String::from(""),
            };

            match (details.reason, details.subcode) {
                // A deliberate exit, so like a successful one, only print a stack trace on request.
                (0x20026, Some(status)) => (
                    monitor_options.always_print_stacktrace,
                    Err(FirmwareExited::WithStatus(status).into()),
                ),
                _ => (
                    true,
                    Err(FirmwareExited::WithReason(format!("{reason}{subcode}")).into()),
                ),
            }
        }
        Err(e) => {
            // Some irrecoverable error happened, probably can't print the stack trace.
            (false, Err(e.into()))
        }
    };

    if print_stack_trace {
        if let Some(path) = path {
            if let Err(error) =
                display_stack_trace(session, path, monitor_options.stack_frame_limit).await
            {
                eprintln!("Can not print stack trace: {error}");
            }
        } else {
            eprintln!("Can not print stack trace because firmware is not available");
        }
    }

    match result {
        Ok(message) => {
            println!("{message}");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// The firmware exited with a non-zero status through semihosting.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum FirmwareExited {
    /// Firmware exited with status {0}
    WithStatus(u32),

    /// Firmware exited: {0}
    WithReason(String),

    /// Firmware stopped unexpectedly: {0}
    Unexpected(String),
}

impl FirmwareExited {
    /// Statuses that don't fit in an exit code map to 1, so a failure never reads as success.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::WithStatus(status) => match u8::try_from(*status) {
                Ok(0) | Err(_) => 1,
                Ok(code) => code,
            },
            _ => 1,
        }
    }
}

/// Describes why the core halted, for a user who runs firmware and does not
/// debug it.
fn describe_halt_reason(reason: WireHaltReason) -> &'static str {
    match reason {
        WireHaltReason::Multiple => "the core halted for multiple reasons",
        WireHaltReason::Breakpoint(WireBreakpointCause::Hardware) => {
            "the core halted on a hardware breakpoint"
        }
        WireHaltReason::Breakpoint(WireBreakpointCause::Software) => {
            "the core halted on a software breakpoint"
        }
        WireHaltReason::Breakpoint(WireBreakpointCause::Unknown) => {
            "the core halted on a breakpoint"
        }
        WireHaltReason::Breakpoint(WireBreakpointCause::Semihosting(_)) => {
            "the core halted on an unsupported semihosting command"
        }
        WireHaltReason::Exception => "the core halted on an exception",
        WireHaltReason::Watchpoint => "the core halted on a watchpoint",
        WireHaltReason::Step => "the core halted after a single step",
        WireHaltReason::Request => "the core halted on a debugger request",
        WireHaltReason::External => "the core halted on an external request",
        WireHaltReason::Unknown => "the core halted for an unknown reason",
    }
}

pub async fn test(
    session: &SessionInterface,
    boot_info: BootInfo,
    elf_info: EmbeddedTestElfInfo,
    libtest_args: libtest_mimic::Arguments,
    monitor_options: &MonitoringOptions,
    path: &Path,
    mut output: TargetOutput,
) -> anyhow::Result<()> {
    tracing::info!("libtest args {:?}", libtest_args);
    let token = CancellationToken::new();

    let mut target_output_files =
        connect_target_output_files(&monitor_options.target_output_file).await?;

    let semihosting_options = parse_semihosting_options(&monitor_options.semihosting_file)?;

    let capture = !libtest_args.nocapture;
    let strip_colors = libtest_args.format == Some(libtest_mimic::FormatSetting::Json);

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<TestLogEvent>();

    let rtt_handle = output.rtt_handle();
    let test = async {
        let tests = if elf_info.version == 0 {
            // In embedded test < 0.7, we have to query the tests from the target via semihosting
            sender.send(TestLogEvent::TestStart).unwrap();
            let result = session
                .list_tests(
                    boot_info,
                    rtt_handle,
                    semihosting_options.clone(),
                    async |msg| sender.send(TestLogEvent::Monitor(msg)).unwrap(),
                )
                .await;

            let (reply, captured) = tokio::sync::oneshot::channel();
            sender.send(TestLogEvent::TestEnd(reply)).unwrap();
            let output = captured.await.unwrap_or_default();

            // The output can tell why the list failed.
            match result {
                Ok(tests) => tests.tests,
                Err(e) => {
                    print!("{output}");
                    return Err(e.into());
                }
            }
        } else {
            // Recent embedded test versions report the tests directly via the elf file
            elf_info.tests
        };

        if token.is_cancelled() {
            return Ok(());
        }

        let error = Arc::new(std::sync::Mutex::new(None));
        let tests = tests
            .into_iter()
            .map(|test| {
                create_trial(
                    session,
                    path,
                    rtt_handle,
                    semihosting_options.clone(),
                    sender.clone(),
                    &token,
                    &error,
                    test,
                    monitor_options.stack_frame_limit,
                    capture,
                    strip_colors,
                )
            })
            .collect::<Vec<_>>();

        tokio::task::spawn_blocking(move || {
            let conclusion = libtest_mimic::run(&libtest_args, tests);
            if let Some(error) = error.lock().unwrap().take() {
                return Err(anyhow::Error::from(error));
            }
            if conclusion.has_failed() {
                anyhow::bail!("Some tests failed");
            }

            Ok(())
        })
        .await?
    };

    let log = async {
        log_test_output(
            &mut receiver,
            &mut output,
            &mut target_output_files,
            &monitor_options.rtt_up_channels,
            capture,
        )
        .await;
        futures_util::future::pending().await
    };

    let test_and_log = async {
        tokio::select! {
            result = test => result,
            _ = log => anyhow::bail!("Log task resolved unexpectedly"),
        }
    };

    let result = with_ctrl_c(test_and_log, async {
        token.cancel();
        send_cancel(session).await;
    })
    .await;

    if token.is_cancelled() && monitor_options.always_print_stacktrace {
        display_stack_trace(session, path, monitor_options.stack_frame_limit).await?;
    }

    result
}

#[expect(clippy::too_many_arguments)]
fn create_trial(
    session: &SessionInterface,
    path: &Path,
    rtt_client: Option<Key<RttClient>>,
    semihosting_options: SemihostingOptions,
    sender: UnboundedSender<TestLogEvent>,
    token: &CancellationToken,
    error: &Arc<std::sync::Mutex<Option<ClientError>>>,
    test: Test,
    stack_frame_limit: u32,
    capture: bool,
    strip_colors: bool,
) -> Trial {
    let name = test.name.clone();
    let ignored = test.ignored;
    let path = path.to_path_buf();

    let session = session.clone();
    let token = token.clone();
    let error = error.clone();

    Trial::ignorable_test(name, {
        move || {
            // A cancel or an error ends the test run. The tests that did not run are ignored.
            if token.is_cancelled() || error.lock().unwrap().is_some() {
                return Ok(Completion::ignored_with("not run"));
            }

            let handle = tokio::spawn(async move {
                sender.send(TestLogEvent::TestStart).unwrap();
                let monitor_sender = sender.clone();
                let result = session
                    .run_test(test, rtt_client, semihosting_options, async move |msg| {
                        monitor_sender.send(TestLogEvent::Monitor(msg)).unwrap()
                    })
                    .await;

                let (reply, captured) = tokio::sync::oneshot::channel();
                sender.send(TestLogEvent::TestEnd(reply)).unwrap();
                let mut output = captured.await.unwrap_or_default();

                let reason = match result {
                    Ok(TestResult::Success) => return Ok(Completion::Completed),
                    Ok(TestResult::Cancelled) => {
                        print!("{output}");
                        return Ok(Completion::ignored_with("cancelled"));
                    }
                    Ok(TestResult::Failed(message)) => message,
                    Err(e) => {
                        // The test command returns the error after the test run.
                        let message = e.to_string();
                        *error.lock().unwrap() = Some(e);
                        message
                    }
                };

                let stack_trace = match format_stack_trace(&session, &path, stack_frame_limit).await
                {
                    Ok(stack_trace) => stack_trace,
                    Err(e) => format!("Failed to get the stack trace: {e}\n"),
                };

                if !capture {
                    print!("{stack_trace}");
                    return Err(Failed::from(reason));
                }

                // Same order as libtest: the output, then the failure.
                if !output.is_empty() && !output.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str(&reason);
                output.push('\n');
                output.push_str(stack_trace.trim_end());
                if strip_colors {
                    output = strip_ansi(output);
                }
                Err(Failed::from(output))
            });

            Handle::current().block_on(handle).unwrap()
        }
    })
    .with_ignored_flag(ignored)
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

/// Writes the target output of a test run. In capture mode, collects the output of each test, and
/// drops the output outside of the tests.
async fn log_test_output(
    receiver: &mut UnboundedReceiver<TestLogEvent>,
    output: &mut TargetOutput,
    target_output_files: &mut TargetOutputFiles,
    up_channels: &[u32],
    capture: bool,
) {
    let captured = RefCell::new(None::<CapturedOutput>);
    let write = |message: &str, print: fn(&str)| {
        if !capture {
            print(message);
        } else if let Some(captured) = captured.borrow_mut().as_mut() {
            captured.push(message);
        }
    };

    while let Some(event) = receiver.recv().await {
        match event {
            TestLogEvent::Monitor(event) => {
                handle_monitor_event(
                    output,
                    event,
                    target_output_files,
                    &async |message| write(message, |m| print!("{m}")),
                    &async |message| write(message, |m| eprint!("{m}")),
                    up_channels,
                )
                .await;
            }
            TestLogEvent::TestStart if capture => {
                *captured.borrow_mut() = Some(CapturedOutput::default())
            }
            TestLogEvent::TestStart => {}
            TestLogEvent::TestEnd(reply) => {
                // The next test resets the target, so the decoders cannot complete this data.
                output
                    .flush(
                        target_output_files,
                        &async |message| write(message, |m| print!("{m}")),
                        &async |message| write(message, |m| eprint!("{m}")),
                    )
                    .await;
                let text = captured.take().map(CapturedOutput::into_string);
                _ = reply.send(text.unwrap_or_default());
            }
        }
    }
}

/// A message to the log task of the test runner.
enum TestLogEvent {
    Monitor(MonitorEvent),
    TestStart,
    /// Sends the captured output of the test. The output is empty if capture is off.
    TestEnd(tokio::sync::oneshot::Sender<String>),
}

/// The output of one test. Keeps the last [`Self::LIMIT`] bytes.
#[derive(Default)]
struct CapturedOutput {
    text: String,
    truncated: bool,
}

impl CapturedOutput {
    const LIMIT: usize = 1024 * 1024;

    fn push(&mut self, message: &str) {
        self.text.push_str(message);
        // Trim at twice the limit, so that each push does not move the text.
        if self.text.len() > 2 * Self::LIMIT {
            self.trim();
        }
    }

    fn trim(&mut self) {
        if self.text.len() > Self::LIMIT {
            let start = self.text.ceil_char_boundary(self.text.len() - Self::LIMIT);
            self.text.drain(..start);
            self.truncated = true;
        }
    }

    fn into_string(mut self) -> String {
        self.trim();
        if self.truncated {
            format!("(The start of the output was removed.)\n{}", self.text)
        } else {
            self.text
        }
    }
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

pub struct CliRttClient {
    handle: Key<RttClient>,
    channel_processors: Vec<Channel>,

    // Data necessary to create the channel processors once we know the channel names.
    zephyr_log_dictionary: Option<Arc<ZephyrLogDatabase>>,
}

/// The settings to create the decoders of the target output.
pub struct DecoderSettings {
    log_format: Option<String>,
    show_timestamps: bool,
    show_location: bool,
    timestamp_offset: Option<UtcOffset>,
    defmt_data: Option<DefmtState>,
}

impl DecoderSettings {
    pub fn new(
        meta: &FileMetadata,
        monitor_options: &MonitoringOptions,
        timestamp_offset: Option<UtcOffset>,
    ) -> Self {
        Self {
            log_format: monitor_options.log_format.clone(),
            show_timestamps: !monitor_options.no_timestamps,
            show_location: !monitor_options.no_location,
            timestamp_offset,
            defmt_data: meta.defmt_data.clone(),
        }
    }

    fn defmt_processor(&self) -> Option<DefmtProcessor> {
        self.defmt_data.clone().map(|defmt_data| {
            DefmtProcessor::new(
                defmt_data,
                self.show_timestamps,
                self.show_location,
                self.log_format.as_deref(),
            )
        })
    }

    fn auto_decoder(&self) -> StreamDecoder {
        StreamDecoder::Auto {
            decoder: AutoDecoder::new(
                TextDecoder::new(self.timestamp_offset, self.show_timestamps),
                self.defmt_processor(),
            ),
        }
    }
}

/// The decoders of the target output: RTT, semihosting and the serial port to the target.
pub struct TargetOutput {
    decoders: DecoderSettings,
    rtt_client: Option<CliRttClient>,
    semihosting_streams: HashMap<String, Channel>,
    serial: Channel,
}

impl TargetOutput {
    pub fn new(decoders: DecoderSettings, rtt_client: Option<CliRttClient>) -> Self {
        Self {
            serial: Channel::new("serial".to_string(), decoders.auto_decoder()),
            decoders,
            rtt_client,
            semihosting_streams: HashMap::new(),
        }
    }

    fn semihosting_stream(&mut self, stream: &str) -> &mut Channel {
        self.semihosting_streams
            .entry(stream.to_string())
            .or_insert_with(|| Channel::new(stream.to_string(), self.decoders.auto_decoder()))
    }

    fn rtt_handle(&self) -> Option<Key<RttClient>> {
        self.rtt_client.as_ref().map(CliRttClient::handle)
    }

    /// Writes the data that waits in the decoders for more input.
    async fn flush(
        &mut self,
        target_output_files: &mut TargetOutputFiles,
        shared_writer: &impl AsyncFn(&str),
        error_writer: &impl AsyncFn(&str),
    ) {
        if let Some(client) = &mut self.rtt_client {
            for processor in &mut client.channel_processors {
                let output_file =
                    ChannelIdentifier::Rtt(processor.channel.clone()).find_in(target_output_files);
                processor.flush(shared_writer, output_file).await;
            }
        }
        for (stream, channel) in &mut self.semihosting_streams {
            let output_file =
                ChannelIdentifier::Semihosting(stream.clone()).find_in(target_output_files);
            if stream == "stderr" {
                channel.flush(error_writer, output_file).await;
            } else {
                channel.flush(shared_writer, output_file).await;
            }
        }
        let output_file =
            ChannelIdentifier::Unqualified("serial".to_string()).find_in(target_output_files);
        self.serial.flush(shared_writer, output_file).await;
    }
}

impl CliRttClient {
    pub fn handle(&self) -> Key<RttClient> {
        self.handle
    }

    fn on_channels_discovered(&mut self, up_channels: &[ChannelInfo], decoders: &DecoderSettings) {
        // Already configured.
        if !self.channel_processors.is_empty() {
            return;
        }

        // Zephyr outputs dictionary-based logs on a single channel, which the database records.
        let zephyr_dict_channel = self
            .zephyr_log_dictionary
            .as_ref()
            .map(|db| db.rtt_channel().unwrap_or(0) as usize);

        // Apply our heuristics based on channel names.
        for (number, channel) in up_channels.iter().enumerate() {
            let decoder = if let Some(db) = self
                .zephyr_log_dictionary
                .as_ref()
                .filter(|_| zephyr_dict_channel == Some(number))
            {
                StreamDecoder::ZephyrDict {
                    processor: ZephyrLogDecoder::new(db.clone()),
                }
            } else if channel.name == "defmt" {
                match decoders.defmt_processor() {
                    Some(processor) => StreamDecoder::Defmt { processor },
                    // Not much we can do. Don't silently eat the data.
                    None => StreamDecoder::BinaryLE,
                }
            } else {
                decoders.auto_decoder()
            };

            self.channel_processors
                .push(Channel::new(channel.name.clone(), decoder));
        }

        // If there are multiple channels, print the channel names.
        if up_channels.len() > 1 {
            let width = up_channels.iter().map(|c| c.name.len()).max().unwrap();
            for processor in self.channel_processors.iter_mut() {
                processor.print_channel_name(width);
            }
        }
    }
}

async fn handle_monitor_event(
    output: &mut TargetOutput,
    event: MonitorEvent,
    target_output_files: &mut TargetOutputFiles,
    shared_writer: &impl AsyncFn(&str),
    error_writer: &impl AsyncFn(&str),
    up_channels: &[u32],
) {
    match event {
        MonitorEvent::Rtt(RttEvent::Discovered { up_channels, .. }) => {
            let Some(client) = &mut output.rtt_client else {
                return;
            };

            client.on_channels_discovered(&up_channels, &output.decoders);
        }
        MonitorEvent::Rtt(RttEvent::Output { channel, bytes }) => {
            let Some(client) = &mut output.rtt_client else {
                return;
            };

            if !up_channels.is_empty() && !up_channels.contains(&channel) {
                return;
            }

            let channel = channel as usize;
            let Some(processor) = client.channel_processors.get_mut(channel) else {
                return;
            };

            processor
                .process(
                    &bytes,
                    shared_writer,
                    // See ChannelIdentifier on why we access with clones here; also, while it'd be
                    // more efficient to resolve those lookups at channel discovery, it doesn't really
                    // matter, and again, ease of maintenance beats theoretical performance unless
                    // benchmarked otherwise.
                    ChannelIdentifier::Rtt(processor.channel.clone()).find_in(target_output_files),
                )
                .await;
        }
        MonitorEvent::Semihosting(SemihostingEvent::Output { stream, bytes }) => {
            let output_file =
                ChannelIdentifier::Semihosting(stream.clone()).find_in(target_output_files);

            let channel = output.semihosting_stream(&stream);
            if stream == "stderr" {
                channel.process(&bytes, error_writer, output_file).await;
            } else {
                channel.process(&bytes, shared_writer, output_file).await;
            }
        }
        MonitorEvent::Serial(SerialEvent::Output { bytes }) => {
            let output_file =
                ChannelIdentifier::Unqualified("serial".to_string()).find_in(target_output_files);

            output
                .serial
                .process(&bytes, shared_writer, output_file)
                .await;
        }
    }
}

struct ChannelInfoPrinter<'a>(&'a ChannelInfo);

impl<'a> std::fmt::Display for ChannelInfoPrinter<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (buffer size {})", self.0.name, self.0.buffer_size)
    }
}

struct Channel {
    channel: String,
    decoder: StreamDecoder,
    printer_prefix: String,
}

impl Channel {
    fn new(channel: String, decoder: StreamDecoder) -> Self {
        Self {
            channel,
            decoder,
            printer_prefix: String::new(),
        }
    }

    fn print_channel_name(&mut self, width: usize) {
        self.printer_prefix = format!("[{:width$}] ", self.channel, width = width);
    }

    async fn process(
        &mut self,
        bytes: &[u8],
        shared_writer: &impl AsyncFn(&str),
        copy_to: Option<&mut tokio::fs::File>,
    ) {
        if let Some(data) = self.decoder.process(bytes).ok().flatten() {
            self.write(&data.to_string(), shared_writer, copy_to).await;
        }
    }

    async fn flush(
        &mut self,
        shared_writer: &impl AsyncFn(&str),
        copy_to: Option<&mut tokio::fs::File>,
    ) {
        let data = self.decoder.flush();
        if !data.is_empty() {
            self.write(&data, shared_writer, copy_to).await;
        }
    }

    async fn write(
        &self,
        data: &str,
        shared_writer: &impl AsyncFn(&str),
        copy_to: Option<&mut tokio::fs::File>,
    ) {
        let message = format!("{}{}", self.printer_prefix, data);
        shared_writer(&message).await;
        if let Some(copy_to) = copy_to {
            // Silently discarding output file errors
            _ = copy_to.write_all(data.as_bytes()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sends the semihosting output of each test through the log task. Returns the captured
    /// output of each test.
    async fn capture_tests(tests: &[&[u8]]) -> Vec<String> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut replies = Vec::new();
        for bytes in tests {
            sender.send(TestLogEvent::TestStart).unwrap();
            sender
                .send(TestLogEvent::Monitor(MonitorEvent::Semihosting(
                    SemihostingEvent::Output {
                        stream: "stdout".to_string(),
                        bytes: bytes.to_vec(),
                    },
                )))
                .unwrap();
            let (reply, captured) = tokio::sync::oneshot::channel();
            sender.send(TestLogEvent::TestEnd(reply)).unwrap();
            replies.push(captured);
        }
        drop(sender);

        let decoders = DecoderSettings {
            log_format: None,
            show_timestamps: false,
            show_location: false,
            timestamp_offset: None,
            defmt_data: None,
        };
        let mut output = TargetOutput::new(decoders, None);
        log_test_output(
            &mut receiver,
            &mut output,
            &mut TargetOutputFiles::new(),
            &[],
            true,
        )
        .await;

        let mut captured = Vec::new();
        for reply in replies {
            captured.push(reply.await.unwrap());
        }
        captured
    }

    #[tokio::test]
    async fn incomplete_output_stays_with_its_test() {
        // A trailing 0xFF can be the start of an esp-println defmt frame.
        let captured = capture_tests(&[b"output of the first test\n\xFF", b"second\n"]).await;
        assert_eq!(captured[1], "second\n");
    }

    #[test]
    fn firmware_exit_code() {
        assert_eq!(FirmwareExited::WithStatus(1).exit_code(), 1);
        assert_eq!(FirmwareExited::WithStatus(42).exit_code(), 42);
        assert_eq!(FirmwareExited::WithStatus(255).exit_code(), 255);
        // Must not be reported as success
        assert_eq!(FirmwareExited::WithStatus(256).exit_code(), 1);
        assert_eq!(FirmwareExited::WithStatus(0).exit_code(), 1);
        assert_eq!(FirmwareExited::WithReason(String::from("")).exit_code(), 1);
        assert_eq!(FirmwareExited::Unexpected(String::from("")).exit_code(), 1);
    }

    #[test]
    fn firmware_exit_status_through_anyhow() {
        let error = anyhow::Error::from(FirmwareExited::WithStatus(3)).context("while running");
        let status = error.downcast_ref::<FirmwareExited>().unwrap();
        assert_eq!(status.exit_code(), 3);
    }
}
