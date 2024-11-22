mod normal_run_mode;
use normal_run_mode::*;
mod test_run_mode;
use probe_rs::semihosting::SemihostingCommand;
use test_run_mode::*;

use std::fs::{self, File};
use std::io::{Read, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use probe_rs::debug::{DebugInfo, DebugRegisters};
use probe_rs::flashing::{FileDownloadError, FlashCommitInfo, FormatKind};
use probe_rs::{
    exception_handler_for_core,
    probe::list::Lister,
    rtt::{Error as RttError, ScanRegion},
    Core, CoreInterface, Error, HaltReason, Session, VectorCatchCondition,
};
use signal_hook::consts::signal;
use time::UtcOffset;

use crate::util::common_options::{BinaryDownloadOptions, ProbeOptions};
use crate::util::flash::{build_loader, run_flash_download};
use crate::util::rtt::client::RttClient;
use crate::util::rtt::{ChannelDataCallbacks, RttChannelConfig, RttConfig};
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
    pub(crate) path: PathBuf,

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

    /// The format string to use when printing defmt encoded log messages from the target.
    ///
    /// See https://defmt.ferrous-systems.com/custom-log-output
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

        if !run_download {
            // If we don't have to flash, resume cores now to prevent halting while processing elf
            session.resume_all_cores()?;
        }

        let rtt_scan_regions = match self.shared_options.rtt_scan_memory {
            true => session.target().rtt_scan_regions.clone(),
            false => ScanRegion::Ranges(vec![]),
        };

        let mut rtt_config = RttConfig::default();
        rtt_config.channels.push(RttChannelConfig {
            channel_number: Some(0),
            show_location: !self.shared_options.no_location,
            log_format: self.shared_options.log_format.clone(),
            ..Default::default()
        });

        let format = self
            .shared_options
            .format_options
            .to_format_kind(session.target());
        let elf = if matches!(format, FormatKind::Elf | FormatKind::Idf) {
            Some(fs::read(&self.shared_options.path)?)
        } else {
            None
        };
        let mut rtt_client = RttClient::new(
            elf.as_deref(),
            session.target(),
            rtt_config,
            rtt_scan_regions,
        )?;

        let core_id = rtt_client.core_id();

        let mut should_clear_rtt_header = true;
        if run_download {
            let loader = build_loader(
                &mut session,
                &self.shared_options.path,
                self.shared_options.format_options,
                None,
            )?;

            // When using RTT with a program in flash, the RTT header will be moved to RAM on
            // startup, so clearing it before startup is ok. However, if we're downloading to the
            // header's final address in RAM, then it's not relocated on startup and we should not
            // clear it. This impacts static RTT headers, like used in defmt_rtt.
            if let ScanRegion::Exact(address) = rtt_client.scan_region {
                should_clear_rtt_header = !loader.has_data_for_address(address);
                tracing::debug!("RTT ScanRegion::Exact address is within region to be flashed")
            }

            let flash_commit_info = run_flash_download(
                &mut session,
                &self.shared_options.path,
                &self.shared_options.download_options,
                &probe_options,
                loader,
                self.shared_options.chip_erase,
            )?;

            match flash_commit_info {
                FlashCommitInfo::BootFromRam { entry_point } => {
                    // core should be already reset and halt by this point.
                    session.prepare_running_on_ram(entry_point)?;
                }
                FlashCommitInfo::Other => {
                    // reset the core to leave it in a consistent state after flashing
                    session
                        .core(core_id)?
                        .reset_and_halt(Duration::from_millis(100))?;
                }
            }
        }

        rtt_client.timezone_offset = timestamp_offset;

        if run_download && should_clear_rtt_header {
            // We ended up resetting the MCU, throw away old RTT data and prevent
            // printing warnings when it initialises.
            let mut core = session.core(core_id)?;
            rtt_client.clear_control_block(&mut core)?;
            tracing::debug!("Cleared RTT header");
        } else {
            tracing::debug!("Skipped clearing RTT header")
        }

        run_mode.run(
            session,
            RunLoop {
                core_id,
                path: self.shared_options.path,
                always_print_stacktrace: self.shared_options.always_print_stacktrace,
                rtt_client,
            },
        )?;

        Ok(())
    }
}

trait RunMode {
    fn run(&self, session: Session, run_loop: RunLoop) -> Result<()>;
}

fn detect_run_mode(cmd: &Cmd) -> anyhow::Result<Box<dyn RunMode>> {
    if elf_contains_test(&cmd.shared_options.path)? {
        // We tolerate the run options, even in test mode so that you can set
        // `probe-rs run --catch-hardfault` as cargo runner (used for both unit tests and normal binaries)
        tracing::info!("Detected embedded-test in ELF file. Running as test");
        Ok(TestRunMode::new(&cmd.test_options))
    } else {
        let test_args_specified = cmd.test_options.list
            || cmd.test_options.exact
            || cmd.test_options.format.is_some()
            || !cmd.test_options.filter.is_empty();

        if test_args_specified {
            anyhow::bail!("probe-rs was invoked with arguments exclusive to test mode, but the binary does not contain embedded-test");
        }

        tracing::debug!("No embedded-test in ELF file. Running as normal");
        Ok(NormalRunMode::new(cmd.run_options.clone()))
    }
}

fn elf_contains_test(path: &Path) -> anyhow::Result<bool> {
    let mut file = File::open(path).map_err(FileDownloadError::IO)?;

    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;

    let contains = match goblin::elf::Elf::parse(buffer.as_slice()) {
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
    };

    Ok(contains)
}

struct RunLoop {
    core_id: usize,
    path: PathBuf,
    always_print_stacktrace: bool,
    rtt_client: RttClient,
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
        &mut self,
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

        let result = self.do_run_until(core, output_stream, timeout, start, &mut predicate);

        // Always clean up after RTT but don't overwrite the original result.
        let cleanup_result = self.rtt_client.clean_up(core);

        if result.is_ok() {
            // If the result is Ok, we return the potential error during cleanup.
            cleanup_result?;
        }

        result
    }

    fn do_run_until<F, R>(
        &mut self,
        core: &mut Core,
        output_stream: OutputStream,
        timeout: Option<Duration>,
        start: Instant,
        predicate: &mut F,
    ) -> Result<ReturnReason<R>>
    where
        F: FnMut(HaltReason, &mut Core) -> Result<Option<R>>,
    {
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
            let mut was_halted = false;
            match core.status()? {
                probe_rs::CoreStatus::Halted(reason) => match predicate(reason, core) {
                    Ok(Some(r)) => return_reason = Some(Ok(ReturnReason::Predicate(r))),
                    Err(e) => return_reason = Some(Err(e)),
                    Ok(None) => {
                        was_halted = true;
                        core.run()?
                    }
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

            let had_rtt_data = poll_rtt(&mut self.rtt_client, core, output_stream)?;

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
            // We also poll at 1kHz if the core was halted, to speed up reading strings
            // from semihosting. The core is not expected to be halted for other reasons.
            //
            // If the polling frequency is too high, the USB connection to the probe
            // can become unstable. Hence we only pull as little as necessary.
            if had_rtt_data || was_halted {
                thread::sleep(Duration::from_millis(1));
            } else {
                thread::sleep(Duration::from_millis(100));
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

        let Some(location) = &frame.source_location else {
            continue;
        };

        write!(output_stream, "       ")?;

        write!(output_stream, "{}", location.path.to_path().display())?;

        if let Some(line) = location.line {
            write!(output_stream, ":{line}")?;

            if let Some(col) = location.column {
                let col = match col {
                    probe_rs::debug::ColumnType::LeftEdge => 1,
                    probe_rs::debug::ColumnType::Column(c) => c,
                };
                write!(output_stream, ":{col}")?;
            }
        }

        writeln!(output_stream)?;
    }
    Ok(())
}

/// Poll RTT and print the received buffer.
fn poll_rtt<S: Write + ?Sized>(
    rtt_client: &mut RttClient,
    core: &mut Core<'_>,
    out_stream: &mut S,
) -> Result<bool, anyhow::Error> {
    struct OutCollector<'a, O: Write + ?Sized> {
        out_stream: &'a mut O,
        had_data: bool,
    }

    impl<O: Write + ?Sized> ChannelDataCallbacks for OutCollector<'_, O> {
        fn on_string_data(&mut self, _channel: usize, data: String) -> Result<(), RttError> {
            if data.is_empty() {
                return Ok(());
            }
            self.had_data = true;
            self.out_stream
                .write_all(data.as_bytes())
                .map_err(|err| anyhow!(err))?;
            Ok(())
        }
    }

    let mut out = OutCollector {
        out_stream,
        had_data: false,
    };

    rtt_client.poll(core, &mut out)?;

    Ok(out.had_data)
}

struct SemihostingPrinter {
    stdout_open: bool,
    stderr_open: bool,
}

impl SemihostingPrinter {
    const STDOUT: u32 = 1;
    const STDERR: u32 = 2;

    pub fn new() -> Self {
        Self {
            stdout_open: false,
            stderr_open: false,
        }
    }

    pub fn handle(
        &mut self,
        command: SemihostingCommand,
        core: &mut Core<'_>,
    ) -> Result<(), Error> {
        match command {
            SemihostingCommand::Open(request) => {
                let path = request.path(core)?;
                if path == ":tt" {
                    match request.mode().as_bytes()[0] {
                        b'w' => {
                            self.stdout_open = true;
                            let fd = NonZeroU32::new(Self::STDOUT).unwrap();
                            request.respond_with_handle(core, fd)?;
                        }
                        b'a' => {
                            self.stderr_open = true;
                            let fd = NonZeroU32::new(Self::STDERR).unwrap();
                            request.respond_with_handle(core, fd)?;
                        }
                        other => {
                            tracing::warn!(
                                "Target wanted to open file {path} with mode {mode}, but probe-rs does not support this operation yet. Continuing...",
                                path = path,
                                mode = other
                            );
                        }
                    };
                } else {
                    tracing::warn!(
                        "Target wanted to open file {path}, but probe-rs does not support this operation yet. Continuing..."
                    );
                }
            }
            SemihostingCommand::Close(request) => {
                let handle = request.file_handle(core)?;
                if handle == Self::STDOUT {
                    self.stdout_open = false;
                    request.success(core)?;
                } else if handle == Self::STDERR {
                    self.stderr_open = false;
                    request.success(core)?;
                } else {
                    tracing::warn!(
                        "Target wanted to close file handle {handle}, but probe-rs does not support this operation yet. Continuing..."
                    );
                }
            }
            SemihostingCommand::Write(request) => match request.file_handle() {
                handle if handle == Self::STDOUT => {
                    if self.stdout_open {
                        let bytes = request.read(core)?;
                        let str = String::from_utf8_lossy(&bytes);
                        std::io::stdout().write_all(str.as_bytes()).unwrap();
                        request.write_status(core, 0)?;
                    }
                }
                handle if handle == Self::STDERR => {
                    if self.stderr_open {
                        let bytes = request.read(core)?;
                        let str = String::from_utf8_lossy(&bytes);
                        std::io::stderr().write_all(str.as_bytes()).unwrap();
                        request.write_status(core, 0)?;
                    }
                }
                other => {
                    tracing::warn!(
                        "Target wanted to write to file handle {other}, but probe-rs does not support this operation yet. Continuing...",
                    );
                }
            },
            SemihostingCommand::WriteConsole(request) => {
                std::io::stdout()
                    .write_all(request.read(core)?.as_bytes())
                    .unwrap();
            }

            _ => {}
        };

        Ok(())
    }
}
