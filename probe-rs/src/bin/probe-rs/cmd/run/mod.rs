mod normal_run_mode;
use normal_run_mode::*;

use std::fs::File;
use std::io::Write;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use probe_rs::debug::{DebugInfo, DebugRegisters};
use probe_rs::rtt::ScanRegion;
use probe_rs::{
    exception_handler_for_core, probe::list::Lister, Core, CoreInterface, Error, HaltReason,
    Session, VectorCatchCondition,
};
use probe_rs_target::MemoryRegion;
use signal_hook::consts::signal;
use time::UtcOffset;

use crate::util::common_options::{BinaryDownloadOptions, ProbeOptions};
use crate::util::flash::{build_loader, run_flash_download};
use crate::util::rtt::{self, ChannelDataCallbacks, RttActiveTarget, RttConfig};
use crate::FormatOptions;

const RTT_RETRIES: usize = 10;

#[derive(clap::Parser)]
pub struct Cmd {
    /// Options only used when in normal run mode
    #[clap(flatten)]
    pub(crate) run_options: NormalRunOptions,

    /// Options shared by all run modes
    #[clap(flatten)]
    pub(crate) shared_options: SharedOptions,
}

#[derive(Debug, clap::Parser)]
pub struct SharedOptions {
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
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION")]
    pub(crate) chip_erase: bool,

    /// Suppress filename and line number information from the rtt log
    #[clap(long)]
    pub(crate) no_location: bool,

    #[clap(flatten)]
    pub(crate) format_options: FormatOptions,

    /// The default format string to use for decoding defmt logs.
    #[clap(long)]
    pub(crate) log_format: Option<String>,

    /// Scan the memory to find the RTT control block
    #[clap(long)]
    pub(crate) rtt_scan_memory: bool,
}

// For multi-core targets, it infers the target core from the RTT symbol.
fn get_target_core_id(session: &mut Session, elf_file: &Path) -> usize {
    let maybe_core_id = || {
        let mut file = File::open(elf_file).ok()?;
        let address = RttActiveTarget::get_rtt_symbol(&mut file)?;

        tracing::debug!("RTT symbol found at 0x{:08x}", address);

        let target_memory = session
            .target()
            .memory_map
            .iter()
            .filter_map(|region| {
                if let MemoryRegion::Ram(region) = region {
                    Some(region)
                } else {
                    None
                }
            })
            .find(|region| region.range.contains(&address))?;

        tracing::debug!("RTT symbol is in RAM region {:?}", target_memory.name);

        let core_name = target_memory.cores.first()?;
        let core_id = session
            .target()
            .cores
            .iter()
            .position(|core| core.name == *core_name)?;

        tracing::debug!("RTT symbol is in core {}", core_id);

        Some(core_id)
    };
    maybe_core_id().unwrap_or(0)
}

impl Cmd {
    pub fn run(
        self,
        lister: &Lister,
        run_download: bool,
        timestamp_offset: UtcOffset,
    ) -> Result<()> {
        let run_mode = detect_run_mode(&self)?;

        let (mut session, probe_options) =
            self.shared_options.probe_options.simple_attach(lister)?;
        let path = Path::new(&self.shared_options.path);
        let core_id = get_target_core_id(&mut session, path);

        if run_download {
            let loader = build_loader(&mut session, path, self.shared_options.format_options)?;
            run_flash_download(
                &mut session,
                path,
                &self.shared_options.download_options,
                &probe_options,
                loader,
                self.shared_options.chip_erase,
            )?;

            // reset the core to leave it in a consistent state after flashing
            session
                .core(core_id)?
                .reset_and_halt(Duration::from_millis(100))?;
        }

        let memory_map = session.target().memory_map.clone();
        let rtt_scan_regions = match self.shared_options.rtt_scan_memory {
            true => session.target().rtt_scan_regions.clone(),
            false => Vec::new(),
        };

        run_mode.run(
            session,
            RunLoop {
                core_id,
                memory_map,
                rtt_scan_regions,
                timestamp_offset,
                path: self.shared_options.path,
                always_print_stacktrace: self.shared_options.always_print_stacktrace,
                no_location: self.shared_options.no_location,
                log_format: self.shared_options.log_format,
            },
        )?;

        Ok(())
    }
}

trait RunMode {
    fn run(&self, session: Session, run_loop: RunLoop) -> Result<()>;
}

fn detect_run_mode(cmd: &Cmd) -> Result<Box<dyn RunMode>, anyhow::Error> {
    // We'll add more run modes here as we add support for them.
    // Possible run modes:
    // - TestRunMode (runs embedded-test)
    // - SemihostingArgsRunMode (passes arguments to the target via semihosting)

    Ok(NormalRunMode::new(cmd.run_options.clone()))
}

struct RunLoop {
    core_id: usize,
    memory_map: Vec<MemoryRegion>,
    rtt_scan_regions: Vec<Range<u64>>,
    path: String,
    timestamp_offset: UtcOffset,
    always_print_stacktrace: bool,
    no_location: bool,
    log_format: Option<String>,
}

#[derive(PartialEq, Debug)]
enum ReturnReason<R> {
    /// The user pressed CTRL +C
    User,
    /// The predicate requested a return
    Predicate(R),
    /// Timeout elapsed
    Timeout,
}

impl RunLoop {
    /// Attaches to RTT and runs the core until it halts.
    ///
    /// Upon halt the predicate is invoked with the halt reason:
    /// * If the predicate returns `Ok(Some(r))` the run loop returns `Ok(ReturnReason::Predicate(r))`.
    /// * If the predicate returns `Ok(None)` the run loop will continue running the core.
    /// * If the predicate returns `Err(e)` the run loop will return `Err(e)`.
    ///
    /// The function will also return on timeout with `Ok(ReturnReason::Timeout)` or if the user presses CTRL + C with `Ok(ReturnReason::User)`.
    fn run_until<F, R>(
        &self,
        core: &mut Core,
        catch_hardfault: bool,
        catch_reset: bool,
        timeout: Option<Duration>,
        mut predicate: F,
    ) -> Result<ReturnReason<R>>
    where
        F: FnMut(HaltReason, &mut Core) -> Result<Option<R>>,
    {
        if catch_hardfault || catch_reset {
            core.halt(Duration::from_millis(100))?;

            if catch_hardfault {
                match core.enable_vector_catch(VectorCatchCondition::HardFault) {
                    Ok(_) | Err(Error::NotImplemented(_)) => {} // Don't output an error if vector_catch hasn't been implemented
                    Err(e) => tracing::error!("Failed to enable_vector_catch: {:?}", e),
                }
            }
            if catch_reset {
                match core.enable_vector_catch(VectorCatchCondition::CoreReset) {
                    Ok(_) | Err(Error::NotImplemented(_)) => {} // Don't output an error if vector_catch hasn't been implemented
                    Err(e) => tracing::error!("Failed to enable_vector_catch: {:?}", e),
                }
            }
        }

        if core.core_halted()? {
            core.run()?;
        }
        let start = Instant::now();

        let mut rtt_config = rtt::RttConfig {
            log_format: self.log_format.clone(),
            ..Default::default()
        };
        rtt_config.channels.push(rtt::RttChannelConfig {
            channel_number: Some(0),
            show_location: !self.no_location,
            ..Default::default()
        });

        let mut rtta = attach_to_rtt(
            core,
            self.memory_map.as_slice(),
            self.rtt_scan_regions.as_slice(),
            Path::new(&self.path),
            &rtt_config,
            self.timestamp_offset,
        );

        let exit = Arc::new(AtomicBool::new(false));
        let sig_id = signal_hook::flag::register(signal::SIGINT, exit.clone())?;

        let mut stdout = std::io::stdout();
        let mut return_reason = None;
        while !exit.load(Ordering::Relaxed) && return_reason.is_none() {
            // check for halt first, poll rtt after.
            // this is important so we do one last poll after halt, so we flush all messages
            // the core printed before halting, such as a panic message.
            match core.status()? {
                probe_rs::CoreStatus::Halted(reason) => match predicate(reason, core) {
                    Ok(Some(r)) => return_reason = Some(Ok(ReturnReason::Predicate(r))),
                    Err(e) => return_reason = Some(Err(e)),
                    Ok(None) => core.run()?,
                },
                probe_rs::CoreStatus::Running
                | probe_rs::CoreStatus::Sleeping
                | probe_rs::CoreStatus::Unknown => {
                    // Carry on
                }

                probe_rs::CoreStatus::LockedUp => {
                    return Err(anyhow!("The core is locked up."));
                }
            }

            let had_rtt_data = poll_rtt(&mut rtta, core, &mut stdout)?;

            match timeout {
                Some(timeout) if start.elapsed() >= timeout => {
                    core.halt(Duration::from_secs(1))?;
                    return_reason = Some(Ok(ReturnReason::Timeout));
                    break;
                }
                _ => {}
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

        let return_reason = match return_reason {
            None => {
                // manually halted with Control+C. Stop the core.
                core.halt(Duration::from_secs(1))?;
                Ok(ReturnReason::User)
            }
            Some(r) => r,
        };

        if self.always_print_stacktrace
            || return_reason.is_err()
            || matches!(return_reason, Ok(ReturnReason::Timeout))
        {
            print_stacktrace(core, Path::new(&self.path))?;
        }

        signal_hook::low_level::unregister(sig_id);
        signal_hook::flag::register_conditional_default(signal::SIGINT, exit)?;

        return_reason
    }
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
        struct StdOutCollector<'a> {
            stdout: &'a mut std::io::Stdout,
            had_data: bool,
        }

        impl ChannelDataCallbacks for StdOutCollector<'_> {
            fn on_string_data(
                &mut self,
                _channel: usize,
                data: String,
            ) -> Result<(), anyhow::Error> {
                if data.is_empty() {
                    return Ok(());
                }
                self.had_data = true;
                self.stdout.write_all(data.as_bytes())?;
                Ok(())
            }
        }

        let mut out = StdOutCollector {
            stdout,
            had_data: false,
        };

        rtta.poll_rtt_fallible(core, &mut out)?;
        had_data = out.had_data;
    }

    Ok(had_data)
}

/// Attach to the RTT buffers.
fn attach_to_rtt(
    core: &mut Core<'_>,
    memory_map: &[MemoryRegion],
    scan_regions: &[Range<u64>],
    path: &Path,
    rtt_config: &RttConfig,
    timestamp_offset: UtcOffset,
) -> Option<rtt::RttActiveTarget> {
    let scan_regions = ScanRegion::Ranges(scan_regions.to_vec());
    for _ in 0..RTT_RETRIES {
        match rtt::attach_to_rtt(core, memory_map, &scan_regions, path) {
            Ok(Some(target_rtt)) => {
                let app = RttActiveTarget::new(target_rtt, path, rtt_config, timestamp_offset);

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
