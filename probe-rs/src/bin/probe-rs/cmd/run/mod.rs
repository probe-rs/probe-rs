mod normal_run_mode;
use normal_run_mode::*;
mod test_run_mode;
use test_run_mode::*;

use std::fs::{self, File};
use std::io::{Read, Write};
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use probe_rs::debug::{DebugInfo, DebugRegisters};
use probe_rs::flashing::FileDownloadError;
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
use crate::util::rtt::{
    self, try_attach_to_rtt, ChannelDataCallbacks, DefmtState, RttActiveTarget, RttConfig,
};
use crate::FormatOptions;

#[derive(clap::Parser)]
pub struct Cmd {
    /// Options only used when in normal run mode
    #[clap(flatten)]
    pub(crate) run_options: NormalRunOptions,

    /// Options only used when in test mode
    #[clap(flatten)]
    pub(crate) test_options: TestOptions,

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

    /// The path to the ELF file to flash and run.
    #[clap(
        index = 1,
        help = "The path to the ELF file to flash and run.\n\
    If the binary uses `embedded-test` each test will be executed in turn. See `TEST OPTIONS` for more configuration options exclusive to this mode.\n\
    If the binary does not use `embedded-test` the binary will be flashed and run normally. See `RUN OPTIONS` for more configuration options exclusive to this mode."
    )]
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
        let core_id = rtt::get_target_core_id(&mut session, &self.shared_options.path);

        if run_download {
            let loader = build_loader(
                &mut session,
                &self.shared_options.path,
                self.shared_options.format_options,
                None,
            )?;
            run_flash_download(
                &mut session,
                &self.shared_options.path,
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
    let elf_contains_test = {
        let mut file = match File::open(cmd.shared_options.path.as_str()) {
            Ok(file) => file,
            Err(e) => return Err(FileDownloadError::IO(e)).context("Failed to open binary file."),
        };
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;
        match goblin::elf::Elf::parse(buffer.as_slice()) {
            Ok(elf) if elf.syms.is_empty() => {
                tracing::debug!("No Symbols in ELF");
                false
            }
            Ok(elf) => elf
                .syms
                .iter()
                .any(|sym| elf.strtab.get_at(sym.st_name) == Some("EMBEDDED_TEST_VERSION")),
            Err(_) => {
                tracing::debug!("Failed to parse ELF file");
                false
            }
        }
    };

    if elf_contains_test {
        // We tolerate the run options, even in test mode so that you can set `probe-rs run --catch-hardfault` as cargo runner (used for both unit tests and normal binaries)
        tracing::info!("Detected embedded-test in ELF file. Running as test");
        Ok(TestRunMode::new(&cmd.test_options))
    } else {
        let test_args_specified = cmd.test_options.list
            || cmd.test_options.exact
            || cmd.test_options.format.is_some()
            || !cmd.test_options.filter.is_empty();
        if test_args_specified {
            return Err(anyhow!("probe-rs was invoked with arguments exclusive to test mode, but the binary does not contain embedded-test"));
        }
        tracing::debug!("No embedded-test in ELF file. Running as normal");
        Ok(NormalRunMode::new(cmd.run_options.clone()))
    }
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

/// The output stream to print RTT and Stack Traces to
enum OutputStream {
    Stdout,
    Stderr,
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
        output_stream: OutputStream,
        timeout: Option<Duration>,
        mut predicate: F,
    ) -> Result<ReturnReason<R>>
    where
        F: FnMut(HaltReason, &mut Core) -> Result<Option<R>>,
    {
        if catch_hardfault || catch_reset {
            if !core.core_halted()? {
                core.halt(Duration::from_millis(100))?;
            }

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
            Duration::from_secs(1),
            self.memory_map.as_slice(),
            &ScanRegion::Ranges(self.rtt_scan_regions.clone()),
            Path::new(&self.path),
            &rtt_config,
            self.timestamp_offset,
        )
        .context("Failed to attach to RTT")?;

        let exit = Arc::new(AtomicBool::new(false));
        let sig_id = signal_hook::flag::register(signal::SIGINT, exit.clone())?;

        let mut stdout;
        let mut stderr;
        let output_stream: &mut dyn Write = match output_stream {
            OutputStream::Stdout => {
                stdout = std::io::stdout();
                &mut stdout
            }
            OutputStream::Stderr => {
                stderr = std::io::stderr();
                &mut stderr
            }
        };

        let return_reason = loop {
            // check for halt first, poll rtt after.
            // this is important so we do one last poll after halt, so we flush all messages
            // the core printed before halting, such as a panic message.
            let mut return_reason = None;
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

            let had_rtt_data = poll_rtt(&mut rtta, core, output_stream)?;

            if return_reason.is_none() {
                if exit.load(Ordering::Relaxed) {
                    return_reason = Some(Ok(ReturnReason::User));
                }

                if let Some(timeout) = timeout {
                    if start.elapsed() >= timeout {
                        return_reason = Some(Ok(ReturnReason::Timeout));
                    }
                }
            }

            if let Some(reason) = return_reason {
                break reason;
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
        };

        if self.always_print_stacktrace
            || return_reason.is_err()
            || matches!(return_reason, Ok(ReturnReason::Timeout))
        {
            if !core.core_halted()? {
                core.halt(Duration::from_secs(1))?;
            }
            print_stacktrace(core, Path::new(&self.path), output_stream)?;
        }

        signal_hook::low_level::unregister(sig_id);
        signal_hook::flag::register_conditional_default(signal::SIGINT, exit)?;

        return_reason
    }
}

/// Prints the stacktrace of the current execution state.
fn print_stacktrace<S: Write + ?Sized>(
    core: &mut impl CoreInterface,
    path: &Path,
    output_stream: &mut S,
) -> Result<(), anyhow::Error> {
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
        write!(
            output_stream,
            "Frame {}: {} @ {}",
            i, frame.function_name, frame.pc
        )?;

        if frame.is_inlined {
            write!(output_stream, " inline")?;
        }
        writeln!(output_stream)?;

        if let Some(location) = &frame.source_location {
            if location.directory.is_some() || location.file.is_some() {
                write!(output_stream, "       ")?;

                if let Some(dir) = &location.directory {
                    write!(output_stream, "{}", dir.to_path().display())?;
                }

                if let Some(file) = &location.file {
                    write!(output_stream, "/{file}")?;

                    if let Some(line) = location.line {
                        write!(output_stream, ":{line}")?;

                        if let Some(col) = location.column {
                            match col {
                                probe_rs::debug::ColumnType::LeftEdge => {
                                    write!(output_stream, ":1")?
                                }
                                probe_rs::debug::ColumnType::Column(c) => {
                                    write!(output_stream, ":{c}")?
                                }
                            }
                        }
                    }
                }

                writeln!(output_stream)?;
            }
        }
    }
    Ok(())
}

/// Poll RTT and print the received buffer.
fn poll_rtt<S: Write + ?Sized>(
    rtta: &mut Option<rtt::RttActiveTarget>,
    core: &mut Core<'_>,
    out_stream: &mut S,
) -> Result<bool, anyhow::Error> {
    let mut had_data = false;
    if let Some(rtta) = rtta {
        struct OutCollector<'a, O: Write + ?Sized> {
            out_stream: &'a mut O,
            had_data: bool,
        }

        impl<O: Write + ?Sized> ChannelDataCallbacks for OutCollector<'_, O> {
            fn on_string_data(
                &mut self,
                _channel: usize,
                data: String,
            ) -> Result<(), anyhow::Error> {
                if data.is_empty() {
                    return Ok(());
                }
                self.had_data = true;
                self.out_stream.write_all(data.as_bytes())?;
                Ok(())
            }
        }

        let mut out = OutCollector {
            out_stream,
            had_data: false,
        };

        rtta.poll_rtt_fallible(core, &mut out)?;
        had_data = out.had_data;
    }

    Ok(had_data)
}

fn attach_to_rtt(
    core: &mut Core<'_>,
    timeout: Duration,
    memory_map: &[MemoryRegion],
    rtt_region: &ScanRegion,
    elf_file: &Path,
    rtt_config: &RttConfig,
    timestamp_offset: UtcOffset,
) -> Result<Option<RttActiveTarget>> {
    // Try to find the RTT control block symbol in the ELF file.
    // If we find it, we can use the exact address to attach to the RTT control block. Otherwise, we
    // fall back to the caller-provided scan regions.
    let elf = fs::read(elf_file)?;
    let scan_region = if let Some(address) = RttActiveTarget::get_rtt_symbol_from_bytes(&elf) {
        ScanRegion::Exact(address as u32)
    } else {
        rtt_region.clone()
    };

    let rtt = try_attach_to_rtt(core, timeout, memory_map, &scan_region)?;

    let Some(rtt) = rtt else {
        return Ok(None);
    };

    let defmt_state = DefmtState::try_from_bytes(&elf)?;
    RttActiveTarget::new(core, rtt, defmt_state, rtt_config, timestamp_offset).map(Some)
}
