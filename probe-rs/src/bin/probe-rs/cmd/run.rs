use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use probe_rs::{BreakpointCause, Core, HaltReason, Lister, SemihostingCommand};
use probe_rs_target::MemoryRegion;
use signal_hook::consts::signal;
use time::UtcOffset;

use crate::util::common_options::{BinaryDownloadOptions, ProbeOptions, RunOptions};
use crate::util::flash::{build_loader, run_flash_download};
use crate::util::rtt::{self, poll_rtt, try_attach_to_rtt};
use crate::util::stack_trace::print_stacktrace;
use crate::FormatOptions;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    pub(crate) probe_options: ProbeOptions,

    #[clap(flatten)]
    pub(crate) download_options: BinaryDownloadOptions,

    #[clap(flatten)]
    pub(crate) run_options: RunOptions,

    /// The path to the ELF file to flash and run
    pub(crate) path: String,

    /// Whether to erase the entire chip before downloading
    #[clap(long)]
    pub(crate) chip_erase: bool,

    #[clap(flatten)]
    pub(crate) format_options: FormatOptions,
}

impl Cmd {
    pub fn run(
        self,
        lister: &Lister,
        run_download: bool,
        timestamp_offset: UtcOffset,
    ) -> Result<()> {
        let (mut session, probe_options) = self.probe_options.simple_attach(lister)?;
        let path = Path::new(&self.path);

        if run_download {
            let loader = build_loader(&mut session, path, self.format_options)?;
            run_flash_download(
                &mut session,
                path,
                &self.download_options,
                &probe_options,
                loader,
                self.chip_erase,
            )?;
            // reset the core to leave it in a consistent state after flashing
            session
                .core(0)?
                .reset_and_halt(Duration::from_millis(100))?;
        }

        let memory_map = session.target().memory_map.clone();
        let rtt_scan_regions = match self.run_options.rtt_scan_memory {
            true => session.target().rtt_scan_regions.clone(),
            false => Vec::new(),
        };
        let mut core = session.core(0)?;

        self.run_options.maybe_enable_vector_catch(&mut core)?;

        core.run()?;

        run_loop(
            &mut core,
            &memory_map,
            &rtt_scan_regions,
            path,
            timestamp_offset,
            self.run_options.always_print_stacktrace,
            self.run_options.no_location,
            self.run_options.log_format.as_deref(),
        )?;

        Ok(())
    }
}

/// Print all RTT messages and a stacktrace when the core stops due to an
/// exception or when ctrl + c is pressed.
///
/// Returns `Ok(())` if the core gracefully halted, or an error.
#[allow(clippy::too_many_arguments)]
fn run_loop(
    core: &mut Core<'_>,
    memory_map: &[MemoryRegion],
    rtt_scan_regions: &[Range<u64>],
    path: &Path,
    timestamp_offset: UtcOffset,
    always_print_stacktrace: bool,
    no_location: bool,
    log_format: Option<&str>,
) -> Result<(), anyhow::Error> {
    let mut rtt_config = rtt::RttConfig::default();
    rtt_config.channels.push(rtt::RttChannelConfig {
        channel_number: Some(0),
        show_location: !no_location,
        ..Default::default()
    });

    let mut rtta = try_attach_to_rtt(
        core,
        memory_map,
        rtt_scan_regions,
        path,
        rtt_config,
        timestamp_offset,
        log_format,
    );

    let exit = Arc::new(AtomicBool::new(false));
    let sig_id = signal_hook::flag::register(signal::SIGINT, exit.clone())?;

    let mut stderr = std::io::stderr();
    let mut halt_reason: Option<HaltReason> = None;
    while !exit.load(Ordering::Relaxed) && halt_reason.is_none() {
        // check for halt first, poll rtt after.
        // this is important so we do one last poll after halt, so we flush all messages
        // the core printed before halting, such as a panic message.
        match core.status()? {
            probe_rs::CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(
                SemihostingCommand::Unknown { operation, .. },
            ))) => {
                tracing::error!("Target wanted to run semihosting operation {:#x}, but probe-rs does not support this operation yet. Continuing...", operation);
                core.run()?;
            }
            probe_rs::CoreStatus::Halted(r) => halt_reason = Some(r),
            probe_rs::CoreStatus::Running
            | probe_rs::CoreStatus::LockedUp
            | probe_rs::CoreStatus::Sleeping
            | probe_rs::CoreStatus::Unknown => {
                // Carry on
            }
        }

        let had_rtt_data = poll_rtt(&mut rtta, core, &mut stderr)?;

        // Poll RTT with a frequency of 10 Hz if we do not receive any new data.
        // Once we receive new data, we bump the frequency to 1kHz.
        //
        // If the polling frequency is too high, the USB connection to the probe
        // can become unstable. Hence we only pull as little as necessary.
        if had_rtt_data {
            std::thread::sleep(Duration::from_millis(1));
        } else {
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    let result = match halt_reason {
        None => {
            // manually halted with Control+C. Stop the core.
            core.halt(Duration::from_secs(1))?;
            Ok(())
        }
        Some(reason) => match reason {
            HaltReason::Breakpoint(BreakpointCause::Semihosting(
                SemihostingCommand::ExitSuccess,
            )) => Ok(()),
            HaltReason::Breakpoint(BreakpointCause::Semihosting(
                SemihostingCommand::ExitError { code },
            )) => Err(anyhow!(
                "Semihosting indicates exit with failure code: {code:#08x} ({code})"
            )),
            _ => Err(anyhow!("CPU halted unexpectedly.")),
        },
    };

    if always_print_stacktrace || result.is_err() {
        print_stacktrace(core, path)?;
    }

    signal_hook::low_level::unregister(sig_id);
    signal_hook::flag::register_conditional_default(signal::SIGINT, exit)?;

    result
}
