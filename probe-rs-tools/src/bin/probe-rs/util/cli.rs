//! CLI-specific building blocks.

use std::{future::Future, path::Path, time::Instant};

use anyhow::Context;
use colored::Colorize;
use libtest_mimic::{Failed, Trial};
use time::UtcOffset;
use tokio::{runtime::Handle, sync::mpsc::UnboundedSender};
use tokio_util::sync::CancellationToken;

use crate::{
    FormatOptions,
    rpc::{
        Key,
        client::{RpcClient, SessionInterface},
        functions::{
            CancelTopic,
            flash::{BootInfo, DownloadOptions, FlashLayout, ProgressEvent, VerifyResult},
            monitor::{MonitorEvent, MonitorMode, MonitorOptions, SemihostingOutput},
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
            .verify(loader.loader, |event| {
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
                |event| {
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

pub async fn monitor(
    session: &SessionInterface,
    mode: MonitorMode,
    path: &Path,
    mut rtt_client: CliRttClient,
    options: MonitorOptions,
    print_stack_trace: bool,
) -> anyhow::Result<()> {
    let monitor = session.monitor(mode, options, |msg| {
        print_monitor_event(&mut rtt_client, msg)
    });

    let result = with_ctrl_c(monitor, async {
        session.client().publish::<CancelTopic>(&()).await.unwrap();
    })
    .await;

    if print_stack_trace {
        display_stack_trace(session, path).await?;
    }

    result
}

pub async fn test(
    session: &SessionInterface,
    boot_info: BootInfo,
    libtest_args: libtest_mimic::Arguments,
    print_stack_trace: bool,
    path: &Path,
    mut rtt_client: CliRttClient,
) -> anyhow::Result<()> {
    tracing::info!("libtest args {:?}", libtest_args);
    let token = CancellationToken::new();

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<MonitorEvent>();

    let rtt_handle = rtt_client.handle;
    let test = async {
        let tests = session
            .list_tests(boot_info, rtt_handle, |msg| sender.send(msg).unwrap())
            .await?;

        if token.is_cancelled() {
            return Ok(());
        }

        let tests = tests
            .tests
            .into_iter()
            .map(|test| create_trial(session, path, rtt_handle, sender.clone(), &token, test))
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
            print_monitor_event(&mut rtt_client, event);
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
    rtt_client: Key<RttClient>,
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
                    .run_test(test, rtt_client, |msg| sender.send(msg).unwrap())
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
                        eprintln!("Error: {:?}", e);
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
        println!("Core {}", core);
        for frame in frames {
            println!("    {}", frame);
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
    tokio::select! {
        _ = tokio::signal::ctrl_c() => eprintln!("Received Ctrl+C, exiting"),
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
            let decoder = if channel == "defmt" {
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

fn print_monitor_event(rtt_client: &mut CliRttClient, event: MonitorEvent) {
    match event {
        MonitorEvent::RttDiscovered { up_channels, .. } => {
            rtt_client.on_channels_discovered(&up_channels);
        }
        MonitorEvent::RttOutput { channel, bytes } => {
            let Some(processor) = rtt_client.channel_processors.get_mut(channel as usize) else {
                return;
            };

            processor.process(&bytes);
        }
        MonitorEvent::SemihostingOutput(SemihostingOutput::StdOut(str)) => {
            print!("{}", str)
        }
        MonitorEvent::SemihostingOutput(SemihostingOutput::StdErr(str)) => {
            eprint!("{}", str)
        }
    }
}

struct Channel {
    channel: String,
    decoder: RttDecoder,
    printer: Printer,
}

impl Channel {
    fn new(channel: String, decoder: RttDecoder) -> Self {
        Self {
            channel,
            decoder,
            printer: Printer {
                prefix: String::new(),
            },
        }
    }

    fn print_channel_name(&mut self, width: usize) {
        self.printer.prefix = format!("[{:width$}] ", self.channel, width = width);
    }

    fn process(&mut self, bytes: &[u8]) {
        _ = self.decoder.process(bytes, &mut self.printer);
    }
}

struct Printer {
    prefix: String,
}
impl RttDataHandler for Printer {
    fn on_string_data(&mut self, data: String) -> Result<(), probe_rs::rtt::Error> {
        print!("{}{}", self.prefix, data);
        Ok(())
    }
}
