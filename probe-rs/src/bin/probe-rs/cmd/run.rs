use std::fs::File;
use std::io::Write;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use probe_rs::debug::DebugInfo;
use probe_rs::flashing::{FileDownloadError, Format};
use probe_rs::{BreakpointCause, Core, HaltReason, SemihostingCommand, VectorCatchCondition};
use probe_rs_target::MemoryRegion;
use signal_hook::consts::signal;
use time::UtcOffset;

use crate::util::common_options::{BinaryDownloadOptions, ProbeOptions};
use crate::util::flash::run_flash_download;
use crate::util::rtt::{self, RttConfig};
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
}

impl Cmd {
    pub fn run(self, run_download: bool, timestamp_offset: UtcOffset) -> Result<()> {
        let (mut session, probe_options) = self.probe_options.simple_attach()?;
        let path = Path::new(&self.path);

        if run_download {
            let mut file = match File::open(&self.path) {
                Ok(file) => file,
                Err(e) => {
                    return Err(FileDownloadError::IO(e)).context("Failed to open binary file.")
                }
            };

            let mut loader = session.target().flash_loader();

            let format = self.format_options.into_format()?;
            match format {
                Format::Bin(options) => loader.load_bin_data(&mut file, options),
                Format::Elf => loader.load_elf_data(&mut file),
                Format::Hex => loader.load_hex_data(&mut file),
                Format::Idf(options) => loader.load_idf_data(&mut session, &mut file, options),
                Format::Uf2 => loader.load_uf2_data(&mut file),
            }?;

            run_flash_download(
                &mut session,
                path,
                &self.download_options,
                &probe_options,
                loader,
                self.chip_erase,
            )?;
        }

        let memory_map = session.target().memory_map.clone();
        let rtt_scan_regions = session.target().rtt_scan_regions.clone();
        let mut core = session.core(0)?;

        if run_download {
            core.reset_and_halt(Duration::from_millis(100))?;
            if let Err(e) = core.enable_vector_catch(VectorCatchCondition::All) {
                tracing::error!("Failed to enable_vector_catch: {:?}", e);
            }
            core.run()?;
        }

        run_loop(
            &mut core,
            &memory_map,
            &rtt_scan_regions,
            path,
            timestamp_offset,
            self.always_print_stacktrace,
            self.no_location,
        )?;

        Ok(())
    }
}

/// Print all RTT messsages and a stacktrace when the core stops due to an
/// exception or when ctrl + c is pressed.
///
/// Returns `Ok(())` if the core gracefully halted, or an error.
fn run_loop(
    core: &mut Core<'_>,
    memory_map: &[MemoryRegion],
    rtt_scan_regions: &[Range<u64>],
    path: &Path,
    timestamp_offset: UtcOffset,
    always_print_stacktrace: bool,
    no_location: bool,
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
    );

    let exit = Arc::new(AtomicBool::new(false));
    let sig_id = signal_hook::flag::register(signal::SIGINT, exit.clone())?;

    let mut stdout = std::io::stdout();
    while !exit.load(Ordering::Relaxed) {
        let had_rtt_data = poll_rtt(&mut rtta, core, &mut stdout)?;
        if poll_stacktrace(core, path)? {
            return Ok(());
        }

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
    let manually_halted = exit.load(Ordering::Relaxed);

    if manually_halted {
        core.halt(Duration::from_secs(1))?;
        if always_print_stacktrace {
            poll_stacktrace(core, path)?;
        }
    }

    signal_hook::low_level::unregister(sig_id);
    signal_hook::flag::register_conditional_default(signal::SIGINT, exit)?;
    Ok(())
}

/// Try to fetch the necessary data of the core to print its stacktrace.
///
/// Returns `Ok(true)` if the debuger should stop polling, or `Ok(false)` if the
/// polling should continue, or an error.
fn poll_stacktrace(core: &mut Core<'_>, path: &Path) -> Result<bool> {
    let status = core.status()?;
    let registers = core.registers();
    let pc_register = registers.pc().expect("a program counter register");
    match status {
        probe_rs::CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(
            SemihostingCommand::ExitSuccess,
        ))) => Ok(true),
        probe_rs::CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(
            SemihostingCommand::ExitError { code },
        ))) => Err(anyhow!(
            "Semihosting indicates exit with failure code: {code:#08x} ({code})"
        )),
        probe_rs::CoreStatus::Halted(_reason) => {
            // Try and give the user some info as to why it halted.
            print_stacktrace(core, pc_register, path)?;
            // Report this as an error
            Err(anyhow!("CPU halted unexpectedly."))
        }
        probe_rs::CoreStatus::Running
        | probe_rs::CoreStatus::LockedUp
        | probe_rs::CoreStatus::Sleeping
        | probe_rs::CoreStatus::Unknown => {
            // Carry on
            Ok(false)
        }
    }
}

/// Prints the stacktrace of the current execution state.
fn print_stacktrace(
    core: &mut Core<'_>,
    pc_register: &probe_rs::CoreRegister,
    path: &Path,
) -> Result<(), anyhow::Error> {
    let Some(debug_info) = DebugInfo::from_file(path).ok() else {
        log::error!("No debug info found.");
        return Ok(());
    };
    let program_counter: u64 = core.read_core_reg(pc_register)?;
    let stack_frames = debug_info.unwind(core, program_counter).unwrap();
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
                    print!("{}", dir.display());
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
) -> Option<rtt::RttActiveTarget> {
    for _ in 0..RTT_RETRIES {
        match rtt::attach_to_rtt(
            core,
            memory_map,
            scan_regions,
            path,
            &rtt_config,
            timestamp_offset,
        ) {
            Ok(target_rtt) => return Some(target_rtt),
            Err(error) => {
                log::debug!("{:?} RTT attach error", error);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    log::error!("Failed to attach to RTT continuing...");
    None
}
