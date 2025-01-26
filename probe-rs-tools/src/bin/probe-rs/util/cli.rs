//! CLI-specific building blocks.

use std::{future::Future, path::Path, time::Instant};

use colored::Colorize;
use libtest_mimic::{Failed, Trial};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

use crate::{
    rpc::{
        client::{RpcClient, SessionInterface},
        functions::{
            flash::{BootInfo, DownloadOptions, FlashResult},
            monitor::{MonitorEvent, MonitorMode, MonitorOptions, SemihostingOutput},
            probe::{
                AttachRequest, AttachResult, DebugProbeEntry, DebugProbeSelector, SelectProbeResult,
            },
            stack_trace::StackTrace,
            test::{Test, TestResult},
            CancelTopic,
        },
        Key,
    },
    util::{
        common_options::{BinaryDownloadOptions, ProbeOptions},
        flash::CliProgressBars,
        logging,
        rtt::client::RttClient,
    },
    FormatOptions,
};

pub async fn attach_probe(
    client: &RpcClient,
    mut probe_options: ProbeOptions,
    resume_target: bool,
) -> anyhow::Result<SessionInterface> {
    // Load the chip description if provided.
    if let Some(chip_description) = probe_options.chip_description_path.take() {
        let file = std::fs::File::open(chip_description)?;

        // Load the YAML locally to validate it before sending it to the remote.
        let family_name = probe_rs::config::add_target_from_yaml(file)?;
        let family = probe_rs::config::get_family_by_name(&family_name)?;

        client.load_chip_family(family).await?;
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

pub async fn flash(
    session: &SessionInterface,
    path: &Path,
    chip_erase: bool,
    format: FormatOptions,
    download_options: BinaryDownloadOptions,
    rtt_client: Option<Key<RttClient>>,
) -> anyhow::Result<FlashResult> {
    // Start timer.
    let flash_timer = Instant::now();

    //let flash_layout_output_path = download_options.flash_layout_output_path.clone();
    let pb = if download_options.disable_progressbars {
        None
    } else {
        Some(CliProgressBars::new())
    };

    let options = DownloadOptions {
        keep_unwritten_bytes: download_options.restore_unwritten,
        do_chip_erase: chip_erase,
        skip_erase: false,
        preverify: download_options.preverify,
        verify: download_options.verify,
        disable_double_buffering: download_options.disable_double_buffering,
    };

    let loader = session
        .build_flash_loader(path.to_path_buf(), format)
        .await?;

    let result = session
        .flash(options, loader.loader, rtt_client, move |event| {
            if let Some(ref pb) = pb {
                pb.handle(event);
            }
        })
        .await?;

    // TODO: port visualizer - can't construct FlashLayout outside of the library

    logging::eprintln(format!(
        "     {} in {:.02}s",
        "Finished".green().bold(),
        flash_timer.elapsed().as_secs_f32(),
    ));

    Ok(result)
}

pub async fn monitor(
    session: &SessionInterface,
    mode: MonitorMode,
    path: &Path,
    options: MonitorOptions,
    print_stack_trace: bool,
) -> anyhow::Result<()> {
    let monitor = session.monitor(mode, options, print_monitor_event);

    let mut cancelled = false;

    let result = with_ctrl_c(monitor, async {
        cancelled = true;
        session.client().publish::<CancelTopic>(&()).await.unwrap();
    })
    .await;

    if cancelled && print_stack_trace {
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
    rtt_client: Option<Key<RttClient>>,
) -> anyhow::Result<()> {
    tracing::info!("libtest args {:?}", libtest_args);
    let token = CancellationToken::new();

    let test = async {
        let tests = session
            .list_tests(boot_info, rtt_client, print_monitor_event)
            .await?;

        if token.is_cancelled() {
            return Ok(());
        }

        let tests = tests
            .tests
            .into_iter()
            .map(|test| create_trial(session, path, rtt_client, &token, test))
            .collect::<Vec<_>>();

        tokio::task::spawn_blocking(move || {
            if libtest_mimic::run(&libtest_args, tests).has_failed() {
                anyhow::bail!("Some tests failed");
            }

            Ok(())
        })
        .await?
    };

    let result = with_ctrl_c(test, async {
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
    token: &CancellationToken,
    test: Test,
) -> Trial {
    let name = test.name.clone();
    let ignored = test.ignored;
    let path = path.to_path_buf();

    let session = session.clone();
    let token = token.clone();

    Trial::test(name, move || {
        if token.is_cancelled() {
            eprintln!("Cancelled");
            std::process::exit(0);
        }

        let handle = tokio::spawn(async move {
            match session
                .run_test(test, rtt_client, print_monitor_event)
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

fn print_monitor_event(event: MonitorEvent) {
    match event {
        MonitorEvent::RttOutput(str) => print!("{}", str),
        MonitorEvent::SemihostingOutput(SemihostingOutput::StdOut(str)) => {
            print!("{}", str)
        }
        MonitorEvent::SemihostingOutput(SemihostingOutput::StdErr(str)) => {
            eprint!("{}", str)
        }
    }
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
