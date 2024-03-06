use std::io::Write;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use probe_rs::debug::{DebugInfo, DebugRegisters};
use probe_rs::rtt::ScanRegion;
use probe_rs::{
    exception_handler_for_core, probe::list::Lister, BreakpointCause, Core, CoreInterface, Error,
    HaltReason, SemihostingCommand, VectorCatchCondition,
};
use probe_rs_target::MemoryRegion;
use signal_hook::consts::signal;
use time::UtcOffset;

use crate::util::common_options::{BinaryDownloadOptions, ProbeOptions};
use crate::util::flash::{build_loader, run_flash_download};
use crate::util::rtt::{self, RttActiveTarget, RttConfig};
use crate::FormatOptions;

const RTT_RETRIES: usize = 10;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    pub(crate) probe_options: ProbeOptions,

    #[clap(flatten)]
    pub(crate) download_options: BinaryDownloadOptions,

    /// The path to the ELF file to flash and run
    pub(crate) path: String,

    /// Always print the stacktrace on ctrl + c.
    #[clap(long)]
    pub(crate) always_print_stacktrace: bool,

    /// Whether to erase the entire chip before downloading
    #[clap(long)]
    pub(crate) chip_erase: bool,

    /// Suppress filename and line number information from the rtt log
    #[clap(long)]
    pub(crate) no_location: bool,

    #[clap(flatten)]
    pub(crate) format_options: FormatOptions,

    #[clap(long)]
    pub(crate) log_format: Option<String>,

    /// Enable reset vector catch if its supported on the target.
    #[arg(long)]
    pub catch_reset: bool,
    /// Enable hardfault vector catch if its supported on the target.
    #[arg(long)]
    pub catch_hardfault: bool,

    /// Scan the memory to find the RTT control block
    #[clap(long)]
    pub(crate) rtt_scan_memory: bool,
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
        let rtt_scan_regions = match self.rtt_scan_memory {
            true => session.target().rtt_scan_regions.clone(),
            false => Vec::new(),
        };
        let mut core = session.core(0)?;

        if self.catch_hardfault || self.catch_reset {
            core.halt(Duration::from_millis(100))?;
            if self.catch_hardfault {
                match core.enable_vector_catch(VectorCatchCondition::HardFault) {
                    Ok(_) | Err(Error::NotImplemented(_)) => {} // Don't output an error if vector_catch hasn't been implemented
                    Err(e) => tracing::error!("Failed to enable_vector_catch: {:?}", e),
                }
            }
            if self.catch_reset {
                match core.enable_vector_catch(VectorCatchCondition::CoreReset) {
                    Ok(_) | Err(Error::NotImplemented(_)) => {} // Don't output an error if vector_catch hasn't been implemented
                    Err(e) => tracing::error!("Failed to enable_vector_catch: {:?}", e),
                }
            }
        }

        if core.core_halted()? {
            core.run()?;
        } else {
            run_loop(
                &mut core,
                &memory_map,
                &rtt_scan_regions,
                path,
                timestamp_offset,
                self.always_print_stacktrace,
                self.no_location,
                self.log_format.as_deref(),
            )?;
        }

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

    let mut rtta = attach_to_rtt(
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

    let mut stdout = std::io::stdout();
    let mut halt_reason = None;
    while !exit.load(Ordering::Relaxed) && halt_reason.is_none() {
        // check for halt first, poll rtt after.
        // this is important so we do one last poll after halt, so we flush all messages
        // the core printed before halting, such as a panic message.
        match core.status()? {
            probe_rs::CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(
                SemihostingCommand::Unknown { operation },
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

        let had_rtt_data = poll_rtt(&mut rtta, core, &mut stdout)?;

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

/// Prints the stacktrace of the current execution state.
fn print_stacktrace(core: &mut impl CoreInterface, path: &Path) -> Result<(), anyhow::Error> {
    let Some(debug_info) = DebugInfo::from_file(path).ok() else {
        tracing::error!("No debug info found.");
        return Ok(());
    };
    let initial_registers = DebugRegisters::from_core(core);
    let exception_interface = exception_handler_for_core(core.core_type());
    let instruction_set = core.instruction_set().ok();
    let stack_frames = debug_info
        .unwind(
            core,
            initial_registers,
            exception_interface.as_ref(),
            instruction_set,
        )
        .unwrap();
    for (i, frame) in stack_frames.iter().enumerate() {
        print!("Frame {}: {} @ {}", i, frame.function_name, frame.pc);

        if frame.is_inlined {
            print!(" inline");
        }
        println!();

        if let Some(location) = &frame.source_location {
            if location.directory.is_some() || location.file.is_some() {
                print!("       ");

                if let Some(dir) = &location.directory {
                    print!("{}", dir.to_path().display());
                }

                if let Some(file) = &location.file {
                    print!("/{file}");

                    if let Some(line) = location.line {
                        print!(":{line}");

                        if let Some(col) = location.column {
                            match col {
                                probe_rs::debug::ColumnType::LeftEdge => {
                                    print!(":1")
                                }
                                probe_rs::debug::ColumnType::Column(c) => {
                                    print!(":{c}")
                                }
                            }
                        }
                    }
                }

                println!();
            }
        }
    }
    Ok(())
}

/// Poll RTT and print the received buffer.
fn poll_rtt(
    rtta: &mut Option<rtt::RttActiveTarget>,
    core: &mut Core<'_>,
    stdout: &mut std::io::Stdout,
) -> Result<bool, anyhow::Error> {
    let mut had_data = false;
    if let Some(rtta) = rtta {
        for (_ch, data) in rtta.poll_rtt_fallible(core)? {
            if !data.is_empty() {
                had_data = true;
            }
            stdout.write_all(data.as_bytes())?;
        }
    };
    Ok(had_data)
}

/// Attach to the RTT buffers.
fn attach_to_rtt(
    core: &mut Core<'_>,
    memory_map: &[MemoryRegion],
    scan_regions: &[Range<u64>],
    path: &Path,
    rtt_config: RttConfig,
    timestamp_offset: UtcOffset,
    log_format: Option<&str>,
) -> Option<rtt::RttActiveTarget> {
    let scan_regions = ScanRegion::Ranges(scan_regions.to_vec());
    for _ in 0..RTT_RETRIES {
        match rtt::attach_to_rtt(core, memory_map, &scan_regions, path) {
            Ok(Some(target_rtt)) => {
                let app = RttActiveTarget::new(
                    target_rtt,
                    path,
                    &rtt_config,
                    timestamp_offset,
                    log_format,
                );

                match app {
                    Ok(app) => return Some(app),
                    Err(error) => tracing::debug!("{:?} RTT attach error", error),
                }
            }
            Ok(None) => return None,
            Err(error) => tracing::debug!("{:?} RTT attach error", error),
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    tracing::error!("Failed to attach to RTT, continuing...");
    None
}
