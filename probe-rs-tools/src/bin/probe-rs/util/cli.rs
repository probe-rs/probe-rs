//! CLI-specific building blocks.

use std::{future::Future, ops::DerefMut, path::Path, time::Instant};

use anyhow::Context;
use colored::Colorize;
use libtest_mimic::{Failed, Trial};
use postcard_rpc::host_client::HostClient;
use postcard_schema::Schema;
use serde::de::DeserializeOwned;
use time::UtcOffset;
use tokio::io::AsyncWriteExt;
use tokio::{runtime::Handle, sync::mpsc::UnboundedSender};
use tokio_util::sync::CancellationToken;

use crate::cmd::run::EmbeddedTestElfInfo;
use crate::rpc::functions::monitor::MonitorExitReason;
use crate::rpc::utils::semihosting::SemihostingOptions;
use crate::{
    FormatOptions,
    rpc::{
        Key,
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
            self, DefmtProcessor, DefmtState, RttDataHandler, RttDecoder, RttSymbolError,
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
        if let Some(fallback) = self.unqualified() {
            if map.contains_key(&fallback) {
                return map.get_mut(&fallback);
            };
        }
        map.get_mut(&Self::CatchAll)
    }
}

/// Splits argument text strings like `['channel1=file-for-c1', 'stdout=some-file', 'defaultfile']` by the
/// `=` signs, mapping the keys to a [`ChannelIdentifier`] (or
/// [`CatchAll`][ChannelIdentifier::CatchAll] when no key present) and opening the values as files
/// in append mode.
pub(crate) async fn connect_target_output_files(
    arg: Vec<String>,
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

pub(crate) fn parse_semihosting_options(arg: Vec<String>) -> anyhow::Result<SemihostingOptions> {
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
    path: &Path,
    mut scan_regions: ScanRegion,
    log_format: Option<String>,
    show_location: bool,
    timestamp_offset: Option<UtcOffset>,
) -> anyhow::Result<CliRttClient> {
    let elf = tokio::fs::read(path)
        .await
        .with_context(|| format!("Failed to read firmware from {}", path.display()))?;

    let mut load_defmt_data = false;
    match rtt::get_rtt_symbol_from_bytes(&elf) {
        // Do not scan the memory for the control block.
        Ok(address) => {
            scan_regions = ScanRegion::Exact(address);
            load_defmt_data = true;
        }
        Err(RttSymbolError::RttSymbolNotFound) => {
            load_defmt_data = true;
        }
        _ => {}
    }

    let defmt_data = if load_defmt_data {
        DefmtState::try_from_bytes(&elf)?
    } else {
        None
    };

    // We don't really know what to configure here, so we just use the defaults: Defmt channels
    // will be set to BlockIfFull, others will not be changed.
    let rtt_client = session.create_rtt_client(scan_regions, vec![]).await?;

    // The actual data processor objects will be created once we have the channel names.
    Ok(CliRttClient {
        handle: rtt_client.handle,
        timestamp_offset,
        show_location,
        channel_processors: vec![],
        defmt_data,
        log_format,
    })
}

pub async fn flash(
    session: &SessionInterface,
    path: &Path,
    chip_erase: bool,
    format: FormatOptions,
    download_options: BinaryDownloadOptions,
    rtt_client: Option<&mut CliRttClient>,
) -> anyhow::Result<BootInfo> {
    // Start timer.
    let flash_timer = Instant::now();

    let options = DownloadOptions {
        keep_unwritten_bytes: download_options.restore_unwritten,
        do_chip_erase: chip_erase,
        skip_erase: false,
        verify: download_options.verify,
        disable_double_buffering: download_options.disable_double_buffering,
    };

    let loader = session
        .build_flash_loader(path.to_path_buf(), format)
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
    if let Some(visualizer_output) = download_options.flash_layout_output_path {
        if let Some(phases) = flash_layout {
            let mut flash_layout = FlashLayout::default();
            for phase_layout in phases {
                flash_layout.merge_from(phase_layout);
            }

            let visualizer = flash_layout.visualize();
            _ = visualizer.write_svg(visualizer_output);
        }
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

pub async fn monitor(
    session: &SessionInterface,
    mode: MonitorMode,
    path: &Path,
    mut rtt_client: Option<CliRttClient>,
    options: MonitorOptions,
    print_stack_trace: bool,
    target_output_files: &mut TargetOutputFiles,
) -> anyhow::Result<()> {
    let monitor = session.monitor(mode, options, async |msg| {
        print_monitor_event(&mut rtt_client.as_mut(), msg, target_output_files).await;
    });

    let result = with_ctrl_c(monitor, async {
        session.client().publish::<CancelTopic>(&()).await.unwrap();
    })
    .await;

    let print_stack_trace = match &result {
        Ok(MonitorExitReason::Success | MonitorExitReason::SemihostingExit(Ok(_))) => {
            println!("Firmware exited successfully");
            print_stack_trace // On success, we only print if the user asked for it.
        }
        Ok(MonitorExitReason::UserExit) => {
            println!("Exited by user request");
            print_stack_trace // On ctrl-c, we only print if the user asked for it.
        }
        Ok(MonitorExitReason::UnexpectedExit(reason)) => {
            println!("Firmware exited unexpectedly: {reason}");
            true
        }
        Ok(MonitorExitReason::SemihostingExit(Err(details))) => {
            let reason = match details.reason {
                // HW vector reason codes
                0x20000 => String::from("Branch through zero"),
                0x20001 => String::from("Undefined instrution"),
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

            true
        }
        Err(_) => false, // Some irrecoverable error happened, probably can't print the stack trace.
    };

    if print_stack_trace {
        display_stack_trace(session, path).await?;
    }

    result.map(|_| ())
}

#[allow(clippy::too_many_arguments)]
pub async fn test(
    session: &SessionInterface,
    boot_info: BootInfo,
    elf_info: EmbeddedTestElfInfo,
    libtest_args: libtest_mimic::Arguments,
    print_stack_trace: bool,
    path: &Path,
    mut rtt_client: Option<CliRttClient>,
    target_output_files: &mut TargetOutputFiles,
    semihosting_options: SemihostingOptions,
) -> anyhow::Result<()> {
    tracing::info!("libtest args {:?}", libtest_args);
    let token = CancellationToken::new();

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
            print_monitor_event(&mut rtt_client.as_mut(), event, target_output_files).await;
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

    if token.is_cancelled() && print_stack_trace {
        display_stack_trace(session, path).await?;
    }

    result
}

fn create_trial(
    session: &SessionInterface,
    path: &Path,
    rtt_client: Option<Key<RttClient>>,
    semihosting_options: SemihostingOptions,
    sender: UnboundedSender<MonitorEvent>,
    token: &CancellationToken,
    test: Test,
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
                        display_stack_trace(&session, &path).await?;

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

async fn display_stack_trace(session: &SessionInterface, path: &Path) -> anyhow::Result<()> {
    let stack_trace = session.stack_trace(path.to_path_buf()).await?;

    for StackTrace { core, frames } in stack_trace.cores.iter() {
        println!("Core {core}");
        for frame in frames {
            println!("    {frame}");
        }
    }

    Ok(())
}

/// Runs a future until complation, running another future when Ctrl+C is received.
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
    show_location: bool,
    timestamp_offset: Option<UtcOffset>,
    defmt_data: Option<DefmtState>,
}

impl CliRttClient {
    pub fn handle(&self) -> Key<RttClient> {
        self.handle
    }

    fn on_channels_discovered(&mut self, up_channels: &[String]) {
        // Already configured.
        if !self.channel_processors.is_empty() {
            return;
        }

        // Apply our heuristics based on channel names.
        for channel in up_channels.iter() {
            let decoder =
                if channel == "defmt" || (self.defmt_data.is_some() && up_channels.len() == 1) {
                    if let Some(defmt_data) = self.defmt_data.clone() {
                        RttDecoder::Defmt {
                            processor: DefmtProcessor::new(
                                defmt_data,
                                self.timestamp_offset.is_some(),
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
                    }
                };

            self.channel_processors
                .push(Channel::new(channel.clone(), decoder));
        }

        // If there are multiple channels, print the channel names.
        if up_channels.len() > 1 {
            let width = up_channels.iter().map(|c| c.len()).max().unwrap();
            for processor in self.channel_processors.iter_mut() {
                processor.print_channel_name(width);
            }
        }
    }
}

async fn print_monitor_event(
    rtt_client: &mut Option<impl DerefMut<Target = CliRttClient>>,
    event: MonitorEvent,
    target_output_files: &mut TargetOutputFiles,
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

            let channel = channel as usize;
            let Some(processor) = client.channel_processors.get_mut(channel) else {
                return;
            };

            processor
                .process(
                    &bytes,
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

    async fn process(&mut self, bytes: &[u8], copy_to: Option<&mut tokio::fs::File>) {
        let mut printer = Printer {
            prefix: &self.printer_prefix,
            copy_to,
        };
        let _ = self.decoder.process(bytes, &mut printer).await;
    }
}

struct Printer<'a> {
    prefix: &'a str,
    copy_to: Option<&'a mut tokio::fs::File>,
}
impl RttDataHandler for Printer<'_> {
    async fn on_string_data(&mut self, data: String) -> Result<(), probe_rs::rtt::Error> {
        print!("{}{}", self.prefix, data);
        if let Some(copy_to) = &mut self.copy_to {
            // Silently discarding output file errors
            _ = copy_to.write_all(data.as_bytes()).await;
        }
        Ok(())
    }
}
