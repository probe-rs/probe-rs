//! CLI-specific building blocks.

use std::fmt::Display;
use std::future::pending;
use std::io::Write;
use std::time::Duration;
use std::{future::Future, ops::DerefMut, path::Path, time::Instant};

use anyhow::Context;
use libtest_mimic::{Failed, Trial};
use probe_rs::meta::ElfMetadata;
use probe_rs::rtt::find_rtt_control_block_and_metadata_in_raw_file;
use ratatui::crossterm::style::Stylize;
use rustyline_async::{Readline, ReadlineError, ReadlineEvent, SharedWriter};
use std::env::VarError;
use time::UtcOffset;
use tokio::io::AsyncWriteExt;
use tokio::sync::futures::Notified;
use tokio::sync::{Mutex, MutexGuard, Notify};
use tokio::{runtime::Handle, sync::mpsc::UnboundedSender};
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
    rtt::{DefmtProcessor, DefmtState, RttDecoder},
};
use probe_rs_rpc::CancelTopic;
use probe_rs_rpc::core_ops::{WireBreakpointCause, WireHaltReason};
use probe_rs_rpc::flash::{BootInfo, DownloadOptions, FlashLayout, ProgressEvent, VerifyResult};
use probe_rs_rpc::format::FormatOptions;
use probe_rs_rpc::monitor::{ChannelInfo, MonitorExitReason};
use probe_rs_rpc::monitor::{MonitorMode, MonitorOptions, RttEvent, SemihostingEvent};
use probe_rs_rpc::probe::{
    AttachRequest, AttachResult, DebugProbeEntry, DebugProbeSelector, SelectProbeResult,
};
use probe_rs_rpc::rtt_client::ScanRegion;
use probe_rs_rpc::rtt_config::RttChannelConfig;
use probe_rs_rpc::semihosting_options::SemihostingOptions;
use probe_rs_rpc::stack_trace::StackTrace;
use probe_rs_rpc::stack_trace::StackTraceFrame;
use probe_rs_rpc::test::{Test, TestResult};
use probe_rs_rpc_client::{MonitorEvent, RpcClient, SessionInterface};

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

    let probe = match select_probe(
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
    }

    let result = with_slow_attach_feedback(client.attach_probe(AttachRequest {
        chip: probe_options.chip.or(elf_meta.chip),
        protocol: probe_options.protocol.map(to_wire_protocol),
        probe,
        speed: probe_options.speed,
        connect_under_reset: probe_options.connect_under_reset,
        dry_run: probe_options.dry_run,
        allow_erase_all: probe_options.allow_erase_all,
        resume_target,
        wait_for_probe: probe_options.attach_timeout,
    }))
    .await?;

    match result {
        AttachResult::Success(session) => Ok(SessionInterface::new(client.clone(), session)),
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

/// How long an attach may run before the user gets a progress indicator.
const ATTACH_FEEDBACK_DELAY: Duration = Duration::from_millis(1500);

/// Displays a spinner while `attach` runs, but only if `attach` is slow.
///
/// An attach that waits for a busy probe can take as long as the configured
/// attach timeout, so without this the CLI looks frozen.
async fn with_slow_attach_feedback<F: Future>(attach: F) -> F::Output {
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

    Ok((
        FileMetadata {
            defmt_data,
            scan_regions,
        },
        elf_meta,
    ))
}

pub async fn rtt_client(
    session: &SessionInterface,
    meta: &FileMetadata,
    monitor_options: &MonitoringOptions,
    timestamp_offset: Option<UtcOffset>,
) -> anyhow::Result<CliRttClient> {
    let scan_regions = match &meta.scan_regions {
        Some(scan_regions) => scan_regions.clone(),
        None => monitor_options.scan_region.clone(),
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
        timestamp_offset,
        show_timestamps: !monitor_options.no_timestamps,
        show_location: !monitor_options.no_location,
        channel_processors: vec![],
        defmt_data: meta.defmt_data.clone(),
        log_format: monitor_options.log_format.clone(),
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

    let run_flash = if download_options.preverify {
        let pb = if download_options.disable_progressbars {
            None
        } else {
            Some(CliProgressBars::new())
        };
        let result = session
            .verify(loader.loader, async |event| {
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

        result == VerifyResult::Mismatch
    } else {
        true
    };

    if run_flash {
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
    }

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
    shared_writer: Option<SharedWriter>,
}
impl MonitorUiState {
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
    mut rtt_client: Option<CliRttClient>,
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
        rtt_client: rtt_client.as_ref().map(|client| client.handle()),
        semihosting_options,
    };

    // The mutex around the context should only be held for a short period of time.
    let ui_context = MonitorUiContext::new(monitor_options.rtt_down_channel);

    let monitor = session.monitor(mode, options, async |msg| {
        let mut client = rtt_client.as_mut();

        if let MonitorEvent::Rtt(RttEvent::Discovered {
            down_channels,
            up_channels,
        }) = &msg
        {
            ui_context
                .update(|state| {
                    state.up_channels = up_channels.clone();
                    state.down_channels = down_channels.clone();
                    state.rtt_client = client.as_ref().map(|client| client.handle());
                })
                .await
        };

        handle_monitor_event(
            &mut client,
            msg,
            &mut target_output_files,
            &async |message| ui_context.lock().await.print(message),
            &monitor_options.rtt_up_channels,
        )
        .await;
    });

    // SIGTERM handler on *nix systems
    let terminate = async {
        #[cfg(unix)]
        {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install signal handler")
                .recv()
                .await;
            eprintln!("Received SIGTERM, exiting");
            session.client().publish::<CancelTopic>(&()).await.unwrap();
        }
        pending().await
    };

    // Gets activated when the RTT client discovers down channels.
    // Displays a prompt and waits for user input.
    async fn cli_with_prompt(session: &SessionInterface, context: &MonitorUiContext) {
        let data = context.lock().await.clone();

        let channel_count = data.down_channels.len() as u32;
        let mut selected_channel = data.selected_down_channel % channel_count;

        let prompt = |channel_idx| {
            Prompt::new(format!(
                "{}> ",
                data.down_channels[channel_idx as usize].name
            ))
            .to_string()
        };

        let Ok((mut rl, sw)) = Readline::new(prompt(selected_channel)) else {
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
                    if let Some(client) = data.rtt_client
                        && let Err(error) = session
                            .send_to_rtt(client, selected_channel, line.into_bytes(), 0)
                            .await
                    {
                        eprintln!("Error sending data to RTT: {:?}", error);
                        break;
                    }
                }
                Ok(ReadlineEvent::Eof) => {
                    if channel_count > 1 {
                        selected_channel = (selected_channel + 1) % channel_count;
                        if let Err(error) = rl.update_prompt(&prompt(selected_channel)) {
                            eprintln!("Error updating prompt: {:?}", error);
                            break;
                        }
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
                } else if locked.down_channels.is_empty() {
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
        session.client().publish::<CancelTopic>(&()).await.unwrap();
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
            println!("Firmware exited successfully");
            // On success, we only print if the user asked for it.
            (monitor_options.always_print_stacktrace, Ok(()))
        }
        Ok(MonitorExitReason::UserExit) => {
            println!("Exited by user request");
            // On ctrl-c, we only print if the user asked for it.
            (monitor_options.always_print_stacktrace, Ok(()))
        }
        Ok(MonitorExitReason::Halted(halt_reason)) => {
            let reason = describe_halt_reason(halt_reason);
            println!("Firmware exited unexpectedly: {reason}");
            (true, Err(anyhow::anyhow!(reason)))
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

            println!("Firmware exited with: {reason}{subcode}");

            (true, Err(anyhow::anyhow!(reason)))
        }
        Err(e) => {
            // Some irrecoverable error happened, probably can't print the stack trace.
            (false, Err(e.into()))
        }
    };

    if print_stack_trace {
        if let Some(path) = path {
            display_stack_trace(session, path, monitor_options.stack_frame_limit).await?;
        } else {
            eprintln!("Can not print stack trace because firmware is not available");
        }
    }

    result
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
    mut rtt_client: Option<CliRttClient>,
) -> anyhow::Result<()> {
    tracing::info!("libtest args {:?}", libtest_args);
    let token = CancellationToken::new();

    let mut target_output_files =
        connect_target_output_files(&monitor_options.target_output_file).await?;

    let semihosting_options = parse_semihosting_options(&monitor_options.semihosting_file)?;

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<MonitorEvent>();

    let rtt_handle = rtt_client.as_ref().map(|rtt| rtt.handle);
    let test = async {
        let tests = if elf_info.version == 0 {
            // In embedded test < 0.7, we have to query the tests from the target via semihosting
            session
                .list_tests(
                    boot_info,
                    rtt_handle,
                    semihosting_options.clone(),
                    async |msg| sender.send(msg).unwrap(),
                )
                .await?
                .tests
        } else {
            // Recent embedded test versions report the tests directly via the elf file
            elf_info.tests
        };

        if token.is_cancelled() {
            return Ok(());
        }

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
                    test,
                    monitor_options.stack_frame_limit,
                )
            })
            .collect::<Vec<_>>();

        tokio::task::spawn_blocking(move || {
            if libtest_mimic::run(&libtest_args, tests).has_failed() {
                anyhow::bail!("Some tests failed");
            }

            Ok(())
        })
        .await?
    };

    let log = async {
        while let Some(event) = receiver.recv().await {
            handle_monitor_event(
                &mut rtt_client.as_mut(),
                event,
                &mut target_output_files,
                &async |message| print!("{message}"),
                &monitor_options.rtt_up_channels,
            )
            .await;
        }
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
        session.client().publish::<CancelTopic>(&()).await.unwrap();
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
    sender: UnboundedSender<MonitorEvent>,
    token: &CancellationToken,
    test: Test,
    stack_frame_limit: u32,
) -> Trial {
    let name = test.name.clone();
    let ignored = test.ignored;
    let path = path.to_path_buf();

    let session = session.clone();
    let token = token.clone();

    Trial::test(name, {
        move || {
            if token.is_cancelled() {
                eprintln!("Cancelled");
                std::process::exit(0);
            }

            let handle = tokio::spawn(async move {
                match session
                    .run_test(test, rtt_client, semihosting_options, async move |msg| {
                        sender.send(msg).unwrap()
                    })
                    .await
                {
                    Ok(TestResult::Success) => Ok(()),
                    Ok(TestResult::Cancelled) => {
                        eprintln!("Cancelled");
                        std::process::exit(0);
                    }
                    Ok(TestResult::Failed(message)) => {
                        display_stack_trace(&session, &path, stack_frame_limit).await?;

                        Err(Failed::from(message))
                    }
                    Err(e) => {
                        eprintln!("Error: {e:?}");
                        std::process::exit(1);
                    }
                }
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
    let stack_trace = session
        .stack_trace(path.to_path_buf(), stack_frame_limit)
        .await?;

    for StackTrace { core, frames } in stack_trace.cores.iter() {
        println!("Core {core}");
        for (i, frame) in frames.iter().enumerate() {
            println!("    Frame {i}: {}", format_stack_frame(frame, None));
        }
        if frames.len() >= stack_frame_limit as usize {
            println!("Use `--stack-frame-limit` to increase the number of frames displayed.");
        }
    }

    Ok(())
}

/// Formats a single stack frame for display.
///
/// `colorize` controls ANSI styling: `None` uses the `PROBE_RS_COLOR` default,
/// `Some(b)` forces a specific choice (used by DAP handlers that must honor the
/// remote client's `supportsAnsiStyling` capability instead of the server env).
pub(crate) fn format_stack_frame(frame: &StackTraceFrame, colorize: Option<bool>) -> String {
    use std::fmt::Write as _;

    let color = colorize.unwrap_or_else(probe_rs_color_enabled);

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

/// Runs a future until completion, running another future when Ctrl+C is received.
///
/// This function enables cooperative asynchronous cancellation without dropping the future.
async fn with_ctrl_c<F, I>(f: F, on_ctrl_c: I) -> F::Output
where
    F: Future,
    I: Future,
{
    let mut run = std::pin::pin!(f);
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = tokio::signal::ctrl_c() => eprintln!("Received Ctrl+C, exiting"),
        _ = terminate => eprintln!("Received SIGTERM, exiting"),
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
    log_format: Option<String>,
    show_timestamps: bool,
    show_location: bool,
    timestamp_offset: Option<UtcOffset>,
    defmt_data: Option<DefmtState>,
}

impl CliRttClient {
    pub fn handle(&self) -> Key<RttClient> {
        self.handle
    }

    fn on_channels_discovered(&mut self, up_channels: &[ChannelInfo]) {
        // Already configured.
        if !self.channel_processors.is_empty() {
            return;
        }

        // Apply our heuristics based on channel names.
        for channel in up_channels.iter() {
            let decoder = if channel.name == "defmt" {
                if let Some(defmt_data) = self.defmt_data.clone() {
                    RttDecoder::Defmt {
                        processor: DefmtProcessor::new(
                            defmt_data,
                            self.show_timestamps,
                            self.show_location,
                            self.log_format.as_deref(),
                        ),
                    }
                } else {
                    // Not much we can do. Don't silently eat the data.
                    RttDecoder::BinaryLE
                }
            } else {
                RttDecoder::String {
                    timestamp_offset: self.timestamp_offset,
                    last_line_done: false,
                    show_timestamps: self.show_timestamps,
                }
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
    rtt_client: &mut Option<impl DerefMut<Target = CliRttClient>>,
    event: MonitorEvent,
    target_output_files: &mut TargetOutputFiles,
    shared_writer: &impl AsyncFn(&str),
    up_channels: &[u32],
) {
    match event {
        MonitorEvent::Rtt(RttEvent::Discovered { up_channels, .. }) => {
            let Some(client) = rtt_client else {
                return;
            };

            client.on_channels_discovered(&up_channels);
        }
        MonitorEvent::Rtt(RttEvent::Output { channel, bytes }) => {
            let Some(client) = rtt_client else {
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
        MonitorEvent::Semihosting(SemihostingEvent::Output { stream, data }) => {
            match stream.as_str() {
                "stdout" => print!("{data}"),
                "stderr" => eprint!("{data}"),
                _ => {}
            };

            if let Some(remote_processor) =
                ChannelIdentifier::Semihosting(stream).find_in(target_output_files)
            {
                // Silently discarding output file errors
                _ = remote_processor.write_all(data.as_bytes()).await;
            };
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
    decoder: RttDecoder,
    printer_prefix: String,
}

impl Channel {
    fn new(channel: String, decoder: RttDecoder) -> Self {
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
            let data = data.to_string();
            let message = format!("{}{}", self.printer_prefix, data);
            shared_writer(&message).await;
            if let Some(copy_to) = copy_to {
                // Silently discarding output file errors
                _ = copy_to.write_all(data.as_bytes()).await;
            }
        }
    }
}

pub(crate) fn probe_rs_color_enabled() -> bool {
    matches!(
        std::env::var("PROBE_RS_COLOR").as_deref(),
        Err(VarError::NotPresent) | Ok("true" | "1" | "yes" | "on")
    )
}

/// Defines a named style as a `Display` wrapper.
///
/// The style expression lives in one place. By default, each wrapper consults
/// `probe_rs_color_enabled()` (i.e. the `PROBE_RS_COLOR` env var) when rendering.
/// Call sites with a different rendering context — e.g. a DAP handler whose
/// output is interpreted by a remote client — can override that decision with
/// `.colorize(bool)` without having to know about `PROBE_RS_COLOR` at all.
macro_rules! styled {
    ($name:ident($var:ident) => $style:expr) => {
        pub struct $name<S: AsRef<str>> {
            value: S,
            colorize: Option<bool>,
        }

        impl<S: AsRef<str>> $name<S> {
            pub fn new(value: S) -> Self {
                Self {
                    value,
                    colorize: None,
                }
            }

            /// Explicitly turn ANSI styling on/off, bypassing the `PROBE_RS_COLOR` default.
            #[allow(dead_code)]
            pub fn colorize(mut self, colorize: bool) -> Self {
                self.colorize = Some(colorize);
                self
            }
        }

        impl<S: AsRef<str>> Display for $name<S> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                if self.colorize.unwrap_or_else(probe_rs_color_enabled) {
                    let $var = self.value.as_ref();
                    write!(f, "{}", $style)
                } else {
                    f.write_str(self.value.as_ref())
                }
            }
        }
    };
}

styled!(StackTraceFunction(name) => name.bold().cyan());
styled!(StackTraceAddress(addr) => addr.yellow());
styled!(StackTraceInlineMarker(marker) => marker.italic().dark_yellow());
styled!(StackTraceSourceLocation(loc) => loc.dim().grey());
styled!(Prompt(prompt) => prompt.bold().dark_green());
