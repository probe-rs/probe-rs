//! CLI-specific building blocks.

use std::fmt::Display;
use std::future::pending;
use std::io::Write;
use std::{future::Future, ops::DerefMut, path::Path, time::Instant};

use anyhow::Context;
use libtest_mimic::{Failed, Trial};
use postcard_rpc::host_client::HostClient;
use postcard_schema::Schema;
use probe_rs::rtt::{self, find_rtt_control_block_in_raw_file};
use ratatui::crossterm::style::Stylize;
use rustyline_async::{Readline, ReadlineError, ReadlineEvent, SharedWriter};
use serde::de::DeserializeOwned;
use std::env::VarError;
use time::UtcOffset;
use tokio::io::AsyncWriteExt;
use tokio::sync::futures::Notified;
use tokio::sync::{Mutex, MutexGuard, Notify};
use tokio::{runtime::Handle, sync::mpsc::UnboundedSender};
use tokio_util::sync::CancellationToken;

use crate::cmd::run::{EmbeddedTestElfInfo, MonitoringOptions};
use crate::rpc::Key;
use crate::rpc::functions::monitor::{ChannelInfo, MonitorExitReason};
use crate::rpc::utils::semihosting::SemihostingOptions;
use crate::{
    FormatOptions,
    rpc::{
        client::{MultiSubscribeError, MultiSubscription, MultiTopic, RpcClient, SessionInterface},
        functions::{
            CancelTopic, RttTopic, SemihostingTopic,
            flash::{BootInfo, DownloadOptions, FlashLayout, ProgressEvent, VerifyResult},
            monitor::{MonitorMode, MonitorOptions, RttEvent, SemihostingEvent},
            probe::{
                AttachRequest, AttachResult, DebugProbeEntry, DebugProbeSelector, SelectProbeResult,
            },
            rtt_client::ScanRegion,
            stack_trace::StackTrace,
            test::{Test, TestResult},
        },
    },
    util::{
        common_options::{BinaryDownloadOptions, ProbeOptions},
        flash::CliProgressBars,
        logging,
        rtt::{
            DefmtProcessor, DefmtState, RttChannelConfig, RttDataHandler, RttDecoder,
            client::RttClient,
        },
    },
};

type TargetOutputFiles = std::collections::HashMap<ChannelIdentifier, tokio::fs::File>;

pub async fn attach_probe(
    client: &RpcClient,
    mut probe_options: ProbeOptions,
    resume_target: bool,
) -> anyhow::Result<SessionInterface> {
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

        // Load the YAML locally to validate it before sending it to the remote.
        // We may also need it locally.
        client.registry().await.add_target_family_from_yaml(&file)?;

        client.load_chip_family(file).await?;
    }

    let probe = select_probe(client, probe_options.probe.map(Into::into)).await?;

    let result = client
        .attach_probe(AttachRequest {
            chip: probe_options.chip,
            protocol: probe_options.protocol.map(Into::into),
            probe,
            speed: probe_options.speed,
            connect_under_reset: probe_options.connect_under_reset,
            dry_run: probe_options.dry_run,
            allow_erase_all: probe_options.allow_erase_all,
            resume_target,
        })
        .await?;

    match result {
        AttachResult::Success(sessid) => Ok(SessionInterface::new(client.clone(), sessid)),
        AttachResult::ProbeNotFound => anyhow::bail!("Probe not found"),
        AttachResult::FailedToOpenProbe(error) => anyhow::bail!("Failed to open probe: {error}"),
        AttachResult::ProbeInUse => anyhow::bail!("Probe is already in use"),
    }
}

pub async fn select_probe(
    client: &RpcClient,
    probe: Option<DebugProbeSelector>,
) -> anyhow::Result<DebugProbeEntry> {
    use anyhow::Context as _;
    use std::io::Write as _;

    match client.select_probe(probe).await? {
        SelectProbeResult::Success(probe) => Ok(probe),
        SelectProbeResult::MultipleProbes(list) => {
            println!("Available Probes:");
            for (i, probe_info) in list.iter().enumerate() {
                println!("{i}: {probe_info}");
            }

            print!("Selection: ");
            std::io::stdout().flush().unwrap();

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
        let key;
        let value;
        match parts[..] {
            // Tolerating empty entries in particular makes a trailing comma tolerated.
            [] => continue,
            [single] => {
                key = ChannelIdentifier::CatchAll;
                value = single;
            }
            [first, second] => {
                key = first.parse()?;
                value = second;
            }
            _ => unreachable!("splitn produces at most 2 items."),
        }
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

pub async fn rtt_client(
    session: &SessionInterface,
    path: Option<&Path>,
    monitor_options: &MonitoringOptions,
    timestamp_offset: Option<UtcOffset>,
) -> anyhow::Result<CliRttClient> {
    let elf = if let Some(path) = path {
        tokio::fs::read(path)
            .await
            .with_context(|| format!("Failed to read firmware from {}", path.display()))?
    } else {
        vec![]
    };

    let mut scan_regions = monitor_options.scan_region.clone();
    let mut load_defmt_data = false;
    if let Ok(opt_address) = find_rtt_control_block_in_raw_file(&elf) {
        match opt_address {
            Some(addr) => {
                scan_regions = ScanRegion::Exact(addr);
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
        defmt_data,
        log_format: monitor_options.log_format.clone(),
    })
}

pub async fn flash(
    session: &SessionInterface,
    path: &Path,
    format: FormatOptions,
    download_options: BinaryDownloadOptions,
    rtt_client: Option<&mut CliRttClient>,
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
        skip_reset: download_options.skip_reset,
    };

    options.sanitize();

    let loader = session
        .build_flash_loader(
            path.to_path_buf(),
            format,
            image_target,
            download_options.read_flasher_rtt,
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
            .flash(
                options,
                loader.loader,
                rtt_client.as_ref().map(|c| c.handle),
                async |event| {
                    if let ProgressEvent::FlashLayoutReady {
                        flash_layout: layout,
                    } = &event
                    {
                        flash_layout = Some(layout.clone());
                    }
                    if let Some(ref pb) = pb {
                        pb.handle(event);
                    }
                },
            )
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

        let visualizer = flash_layout.visualize();
        _ = visualizer.write_svg(visualizer_output);
    }

    logging::eprintln(format!(
        "     {} in {:.02}s",
        "Finished".green().bold(),
        flash_timer.elapsed().as_secs_f32(),
    ));

    Ok(loader.boot_info)
}

pub enum MonitorEvent {
    Rtt(RttEvent),
    Semihosting(SemihostingEvent),
}

impl MultiTopic for MonitorEvent {
    type Message = Self;
    type Subscription = MonitorSubscription;

    async fn subscribe<E>(
        client: &HostClient<E>,
        depth: usize,
    ) -> Result<Self::Subscription, MultiSubscribeError>
    where
        E: DeserializeOwned + Schema,
    {
        // TODO: remove MonitorEvent from the RPC interface, split this subscribe into two:
        // one for RTT, one for semihosting, then introduce a MultiSubscription impl for them
        let rtt = RttTopic::subscribe(client, depth).await?;
        let semihosting = SemihostingTopic::subscribe(client, depth).await?;
        Ok(MonitorSubscription { rtt, semihosting })
    }
}

pub struct MonitorSubscription {
    rtt: <RttTopic as MultiTopic>::Subscription,
    semihosting: <SemihostingTopic as MultiTopic>::Subscription,
}
impl MultiSubscription for MonitorSubscription {
    type Message = MonitorEvent;

    async fn next(&mut self) -> Option<Self::Message> {
        tokio::select! {
            message = self.rtt.recv() => message.map(MonitorEvent::Rtt),
            message = self.semihosting.recv() => message.map(MonitorEvent::Semihosting),
        }
    }
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
    catch_reset: bool,
    catch_hardfault: bool,
) -> anyhow::Result<()> {
    let semihosting_options = parse_semihosting_options(&monitor_options.semihosting_file)?;
    let mut target_output_files =
        connect_target_output_files(&monitor_options.target_output_file).await?;

    let options = MonitorOptions {
        catch_reset,
        catch_hardfault,
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
            && !down_channels.is_empty()
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
            Prompt(format!(
                "{}> ",
                &data.down_channels[channel_idx as usize].name
            ))
            .to_string()
        };

        let Ok((mut rl, sw)) = Readline::new(prompt(selected_channel)) else {
            eprintln!("Failed to create readline");
            _ = tokio::signal::ctrl_c().await;

            eprintln!("Received Ctrl+C, exiting");
            return;
        };

        context
            .update(|data| data.shared_writer = Some(sw.clone()))
            .await;

        rl.should_print_line_on(true, false);
        loop {
            match rl.readline().await {
                Ok(ReadlineEvent::Line(line)) => {
                    rl.add_history_entry(line.clone());
                    if let Some(client) = data.rtt_client
                        && let Err(error) = session
                            .send_to_rtt(client, selected_channel, line.into_bytes())
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
                } else if locked.down_channels.is_empty() {
                    DisplayMode::OutputOnly
                } else if monitor_options.list_rtt {
                    DisplayMode::ListChannelsAndQuit
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
                    let mut data = ui_context.lock().await;
                    println!("Up channels:");
                    for (i, channel) in data.up_channels.iter().enumerate() {
                        println!("  {}: {}", i, ChannelInfoPrinter(channel));
                    }
                    println!("Down channels:");
                    for (i, channel) in data.down_channels.iter().enumerate() {
                        println!("  {}: {}", i, ChannelInfoPrinter(channel));
                    }
                    data.exit();
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
        Ok(MonitorExitReason::Success | MonitorExitReason::SemihostingExit(Ok(_))) => {
            println!("Firmware exited successfully");
            // On success, we only print if the user asked for it.
            (monitor_options.always_print_stacktrace, Ok(()))
        }
        Ok(MonitorExitReason::UserExit) => {
            println!("Exited by user request");
            // On ctrl-c, we only print if the user asked for it.
            (monitor_options.always_print_stacktrace, Ok(()))
        }
        Ok(MonitorExitReason::UnexpectedExit(reason)) => {
            println!("Firmware exited unexpectedly: {reason}");
            (true, Err(anyhow::anyhow!("{reason}")))
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
            (false, Err(e))
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
        for frame in frames {
            println!("    {frame}");
        }
        if frames.len() >= stack_frame_limit as usize {
            println!("Use `--stack-frame-limit` to increase the number of frames displayed.");
        }
    }

    Ok(())
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
        let mut printer = Printer {
            prefix: &self.printer_prefix,
            copy_to,
            shared_writer,
        };
        let _ = self.decoder.process(bytes, &mut printer).await;
    }
}

struct Printer<'a, P: AsyncFn(&str)> {
    prefix: &'a str,
    copy_to: Option<&'a mut tokio::fs::File>,
    shared_writer: &'a P,
}
impl<P: AsyncFn(&str)> RttDataHandler for Printer<'_, P> {
    async fn on_string_data(&mut self, data: String) -> Result<(), rtt::Error> {
        let message = format!("{}{}", self.prefix, data);
        (self.shared_writer)(&message).await;
        if let Some(copy_to) = &mut self.copy_to {
            // Silently discarding output file errors
            _ = copy_to.write_all(data.as_bytes()).await;
        }
        Ok(())
    }
}

macro_rules! styled {
    ($name:ident($var:ident) => $style:expr) => {
        pub struct $name<S: AsRef<str>>(pub S);

        impl<S: AsRef<str>> Display for $name<S> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                if matches!(
                    std::env::var("PROBE_RS_COLOR").as_deref(),
                    Err(VarError::NotPresent) | Ok("true" | "1" | "yes" | "on")
                ) {
                    let $var = self.0.as_ref();
                    write!(f, "{}", $style)
                } else {
                    f.write_str(self.0.as_ref())
                }
            }
        }
    };
}

styled!(Prompt(prompt) => prompt.bold().dark_green());
