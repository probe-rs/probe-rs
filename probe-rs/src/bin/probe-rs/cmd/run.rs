use std::fs::File;
use std::io::{Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use libtest_mimic::{Arguments, Failed, FormatSetting, Trial};
use probe_rs::debug::{DebugInfo, DebugRegisters};
use probe_rs::flashing::FileDownloadError;
use probe_rs::rtt::ScanRegion;
use probe_rs::{
    exception_handler_for_core, probe::list::Lister, BreakpointCause, Core, CoreInterface, Error,
    HaltReason, SemihostingCommand, Session, VectorCatchCondition,
};
use probe_rs_target::MemoryRegion;
use signal_hook::consts::signal;
use time::UtcOffset;

use crate::util::common_options::{BinaryDownloadOptions, ProbeOptions};
use crate::util::flash::{build_loader, run_flash_download};
use crate::util::rtt::{self, ChannelDataCallbacks, RttActiveTarget, RttConfig};
use crate::FormatOptions;

const RTT_RETRIES: usize = 10;

#[derive(clap::Parser, Debug)]
pub struct Cmd {
    ///The path to the ELF file to flash and run.
    #[clap(help = "The path to the ELF file to flash and run.\n\
    If the binary uses `embedded-test` each test will be executed in turn. See `TEST OPTIONS` for more configuration options exclusive to this mode.\n\
    If the binary does not use `embedded-test` the binary will be flashed and run normally. See `RUN OPTIONS` for more configuration options exclusive to this mode.")]
    pub(crate) path: String,

    /// Options only used when in non-test mode
    #[clap(flatten)]
    pub(crate) run_options: RunOptions,

    /// Options only used when in test mode
    #[clap(flatten)]
    pub(crate) test_options: TestOptions,

    // ---- General Options ahead ----
    #[clap(flatten)]
    pub(crate) common_options: CommonOptions,

    #[clap(flatten)]
    pub(crate) download_options: BinaryDownloadOptions,

    #[clap(flatten)]
    pub(crate) format_options: FormatOptions,

    /// Whether to erase the entire chip before downloading
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION")]
    pub(crate) chip_erase: bool,

    #[clap(flatten)]
    pub(crate) probe_options: ProbeOptions,
}

// Options only used when using normal runs
#[derive(Debug, clap::Parser, Clone)]
pub struct RunOptions {
    /// Enable reset vector catch if its supported on the target.
    #[clap(long, help_heading = "RUN OPTIONS")]
    pub catch_reset: bool,
    /// Enable hardfault vector catch if its supported on the target.
    #[clap(long, help_heading = "RUN OPTIONS")]
    pub catch_hardfault: bool,
}

// Options only used when using test runs
#[derive(Debug, clap::Parser)]
pub struct TestOptions {
    /// Filter string. Only tests which contain this string are run.
    #[clap(
        value_name = "TEST_FILTER",
        help = "The TEST_FILTER string is tested against the name of all tests, and only those tests whose names contain the filter are run. Multiple filter strings may be passed, which will run all tests matching any of the filters.",
        help_heading = "TEST OPTIONS"
    )]
    pub filter: Vec<String>,

    /// Only list all tests
    #[clap(
        long = "list",
        help = "List all tests instead of executing them",
        help_heading = "TEST OPTIONS"
    )]
    pub list: bool,

    #[clap(
        long = "format",
        value_enum,
        value_name = "pretty|terse|json",
        help_heading = "TEST OPTIONS",
        help = "Configure formatting of the test report output"
    )]
    pub format: Option<FormatSetting>,

    /// If set, filters are matched exactly rather than by substring.
    #[clap(long = "exact", help_heading = "TEST OPTIONS")]
    pub exact: bool,

    /// Options which are ignored, but exist for compatibility with libtest.
    /// E.g. so that vscode and intellij can invoke the test runner with the args they are used to
    #[clap(flatten)]
    _no_op: NoOpTestOptions,
}

#[derive(Debug, clap::Parser)]
struct NoOpTestOptions {
    // No-op, ignored (libtest-mimic always runs in no-capture mode)
    #[clap(long = "nocapture", hide = true)]
    nocapture: bool,

    /// No-op, ignored. libtest-mimic does not currently capture stdout.
    #[clap(long = "show-output", hide = true)]
    show_output: bool,

    /// No-op, ignored. Flag only exists for CLI compatibility with libtest.
    #[clap(short = 'Z', hide = true)]
    unstable_flags: Option<String>,
}

// Options used for normal + test runs
#[derive(Debug, clap::Parser)]
pub struct CommonOptions {
    /// Always print the stacktrace on ctrl + c.
    #[clap(long)]
    pub(crate) always_print_stacktrace: bool,

    /// Suppress filename and line number information from the rtt log
    #[clap(long)]
    pub(crate) no_location: bool,

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

        let (mut session, probe_options) = self.probe_options.simple_attach(lister)?;
        let path = PathBuf::from(&self.path);

        if run_download {
            let loader = build_loader(&mut session, &path, self.format_options)?;
            run_flash_download(
                &mut session,
                &path,
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
        let rtt_scan_regions = match self.common_options.rtt_scan_memory {
            true => session.target().rtt_scan_regions.clone(),
            false => Vec::new(),
        };

        run_mode.run(session, RunLoop{
            memory_map,
            rtt_scan_regions,
            path,
            timestamp_offset,
            always_print_stacktrace: self.common_options.always_print_stacktrace,
            no_location: self.common_options.no_location,
            log_format: self.common_options.log_format,
        })?;

        Ok(())
    }
}

trait RunMode {
    fn run(&self, session: Session, run_loop: RunLoop) -> Result<()>;
}

fn detect_run_mode(cmd: &Cmd) ->  Result<Box<dyn RunMode>, anyhow::Error>  {
    let elf_contains_test = {
        // TODO: Improve this detection:
        //  1. read the elf symbol table instead of grepping for a string
        //  2. Warn the user if no debug symbols are present? Or assume not a test?
        let mut file = match File::open(cmd.path.as_str()) {
            Ok(file) => file,
            Err(e) => return Err(FileDownloadError::IO(e)).context("Failed to open binary file."),
        };
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        let needle = b"EMBEDDED_TEST_VERSION";
        buf.windows(needle.len()).any(|window| window == needle)
    };

    // TODO: if elf_contains_test is true there should be no RunOptions present
    //  and if elf_contains_test is false there should be no TestOptions present

    if elf_contains_test{
        tracing::info!("Detected embedded-test in ELF file. Running as test");
        Ok(Box::new(TestRunMode{
            libtest_args:  Arguments {
                test_threads: Some(1), // Avoid parallel execution
                list: cmd.test_options.list,
                exact: cmd.test_options.exact,
                format: cmd.test_options.format,
                filter: if cmd.test_options.filter.is_empty() {
                    None
                } else {
                    Some(cmd.test_options.filter.join(" "))
                },
                ..Arguments::default()
            }
        }))
    } else {
        tracing::debug!("No embedded-test in ELF file. Running as normal");
        Ok(Box::new(NormalRunMode{
            run_options: cmd.run_options.clone()
        }))
    }
}


/// Test run mode
struct TestRunMode {
  libtest_args: Arguments
}

impl TestRunMode{
    /// Asks the target for the tests, and create a "run the test"-closure for each test.
    /// libtest-mimic is in charge of selecting the tests to run based on the filter and other options
    fn create_tests(session_and_runloop_ref: Arc<Mutex<SessionAndRunLoop>>) -> Result<Vec<Trial>> {
        let mut session_and_runloop = session_and_runloop_ref.lock().unwrap();
        let list = Self::list_tests(&mut *session_and_runloop)?;

        let mut tests = Vec::<Trial>::new();
        for t in &list.tests {
            let test = t.clone();
            let session_and_runloop = session_and_runloop_ref.clone();
            tests.push(
                Trial::test(&t.name, move || {
                    let mut session_and_runloop = session_and_runloop.lock().unwrap();
                    Self::run_test(test, &mut session_and_runloop)
                })
                    .with_ignored_flag(t.ignored),
            )
        }
        Ok(tests)
    }

    const SEMIHOSTING_USER_LIST: u32 = 0x100;

    /// Requests all tests from the target via Semihosting back and forth
    /// When the target first invokes SYS_GET_CMDLINE (0x15), we answer "list"
    /// Then, we wait until the target invokes SEMIHOSTING_USER_LIST (0x100) with the json containing all tests
    fn list_tests(session_and_runloop: &mut SessionAndRunLoop) -> Result<Tests> {
        let mut core = session_and_runloop.session.core(0)?;

        let mut cmdline_requested = false;

        session_and_runloop.run_loop.run(&mut core, true,true, Some(Duration::from_secs(5)), |halt_reason: HaltReason, core: &mut Core| {
            match halt_reason {
                HaltReason::Breakpoint(BreakpointCause::Semihosting(cmd)) =>
                    match cmd {
                        SemihostingCommand::GetCommandLine(request) if !cmdline_requested => {
                            tracing::info!("target asked for cmdline. send 'list'");
                            cmdline_requested = true;
                            request.write_command_line_to_target(core, "list")?; //TODO: fix default retreg if this is not called
                            Ok(None) // not done yet
                        }
                        SemihostingCommand::Unknown(details) if details.operation == Self::SEMIHOSTING_USER_LIST  && cmdline_requested
                        => {
                            let buf = details.get_buffer(core)?;
                            let buf = buf.read(core)?;
                            let list: Tests = serde_json::from_slice(&buf[..])?;
                            //TODO: write return reg=0 ?!
                            tracing::info!("got list of tests from target: {:?}", list);
                            if list.version != 1 {
                                Err(anyhow!("Unsupported test list format version: {}", list.version))
                            } else {
                                Ok(Some(list))
                            }
                        },
                        other => Err(anyhow!("Unexpected semihosting command {:?} cmdline_requested: {:?}", other, cmdline_requested))
                    },
                _ => Err(anyhow!("CPU halted unexpectedly."))
            }
        })?.ok_or(anyhow!("The user pressed ctrl+c before the target responded with the test list."))
    }

    /// Runs a single test on the target
    /// When the target first invokes SYS_GET_CMDLINE (0x15), we answer "run <test_name>
    /// Then we wait until the target invokes SYS_EXIT (0x18) or SYS_EXIT_EXTENDED(0x20) with the exit code
    fn run_test(test: Test, session_and_runloop: &mut SessionAndRunLoop) -> std::result::Result<(), Failed> {
        let core = &mut session_and_runloop.session.core(0)?;
        tracing::info!("Running test {}", test.name);
        core.reset_and_halt(Duration::from_millis(100))?;

        let timeout = test.timeout.map(|t| Duration::from_secs(t as u64));
        let timeout = timeout.unwrap_or(Duration::from_secs(60)); // TODO: make global timeout configurable*/

        let mut cmdline_requested = false;

        let ret = session_and_runloop.run_loop.run(core, true,true,  Some(timeout),|halt_reason: HaltReason, core: &mut Core| {
           match halt_reason {
                HaltReason::Breakpoint(BreakpointCause::Semihosting(cmd)) =>
                    match cmd {
                        SemihostingCommand::GetCommandLine(request) if !cmdline_requested => {
                            let cmdline = format!("run {}", test.name);
                            tracing::info!("target asked for cmdline. send '{}'", cmdline.as_str());
                            cmdline_requested = true;
                            request.write_command_line_to_target(core, cmdline.as_str())?; //TODO: fix default retreg if this is not called
                            Ok(None) // not done yet
                        }
                        SemihostingCommand::ExitSuccess if cmdline_requested => Ok(Some(true)),
                        SemihostingCommand::ExitError(_) if cmdline_requested =>Ok(Some(false)),
                        other => Err(anyhow!("Unexpected semihosting command {:?} cmdline_requested: {:?}", other, cmdline_requested))
                    },
                _ => Err(anyhow!("CPU halted unexpectedly."))
            }
        });

        match ret {
            Ok(Some(exit_status))  => {
                let should_exit_successfully = !test.should_panic;
                if exit_status == should_exit_successfully {
                    Ok(())
                } else {
                    if !exit_status {
                        print_stacktrace(core, &session_and_runloop.run_loop.path)?;
                    }
                    Err(Failed::from(format!(
                        "Test should have {} but it {}",
                        if test.should_panic { "panicked" } else { "passed" },
                        if exit_status { "passed" } else { "panicked" }
                    )))
                }
            },
            Ok(None) => {
                tracing::error!("Test {} was aborted by the user", test.name);
                Err(Failed::from("Test was aborted by the user (CTRL +C)"))
            }
            Err(e) if e.downcast_ref::<&str>() == Some(&"Timeout") => {
                // TODO: signal this in a nicer way
                tracing::error!("Test {} timed out", test.name);
                Err(Failed::from(format!("Test timed out after {:?}", timeout)))
            }
            Err(e) => {
                eprintln!("Error: {:?}", e);
                std::process::exit(1);
            }
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct Tests {
    pub version: u32,
    pub tests: Vec<Test>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Test {
    pub name: String,
    pub should_panic: bool,
    pub ignored: bool,
    pub timeout: Option<u32>,
}


impl RunMode for TestRunMode{
    fn run(&self, session: Session, run_loop: RunLoop) -> Result<()> {
        tracing::info!("libtest args {:?}", self.libtest_args);

        // Unfortunately libtest-mimic wants test functions to live for 'static, so we need to use a mutex to share the session and runloop
        let session_and_runloop = Arc::new(Mutex::new(SessionAndRunLoop {
            session,
            run_loop
        }));

        let tests = Self::create_tests(session_and_runloop)?;
        if libtest_mimic::run(&self.libtest_args, tests).has_failed() {
            Err(anyhow!("Some tests failed"))
        } else {
            Ok(())
        }
    }
}

struct SessionAndRunLoop {
    session: Session,
    run_loop: RunLoop
}


/// Normal run mode (non-test)
struct NormalRunMode{
    run_options: RunOptions
}

impl RunMode for NormalRunMode {
    fn run(&self, mut session: Session, run_loop: RunLoop) -> Result<()> {
        let mut core = session.core(0)?;

        run_loop.run(&mut core, self.run_options.catch_hardfault, self.run_options.catch_reset, None, |halt_reason: HaltReason, _core : &mut Core| {
            match halt_reason {
                HaltReason::Breakpoint(BreakpointCause::Semihosting(cmd)) =>
                    match cmd {
                        SemihostingCommand::ExitSuccess => Ok(Some(())),
                        SemihostingCommand::ExitError(details) => Err(anyhow!("Semihosting indicates exit with {}", details)),
                        SemihostingCommand::Unknown(details) => {
                            tracing::warn!("Target wanted to run semihosting operation {:#x} with parameter {:#x},\
                             but probe-rs does not support this operation yet. Continuing...", details.operation, details.parameter);
                            Ok(None)
                        },
                        SemihostingCommand::GetCommandLine(_) => {
                            tracing::warn!("Target wanted to run semihosting operation SYS_GET_CMDLINE, but probe-rs does not support this operation yet. Continuing...");
                            Ok(None)
                        }
                    },
                _ => Err(anyhow!("CPU halted unexpectedly."))
            }
        })?;
        Ok(())
    }
}

struct RunLoop {
    memory_map: Vec<MemoryRegion>,
    rtt_scan_regions: Vec<Range<u64>>,
    path: PathBuf,
    timestamp_offset: UtcOffset,
    always_print_stacktrace: bool,
    no_location: bool,
    log_format: Option<String>,
}

impl RunLoop {
    /// Attaches to RTT and runs the core until it halts or the user presses CTRL+C.
    /// If the user presses CTRL+C the core is stopped and `Ok(None)` is returned.
    /// When the core halts otherwise, the `halt_handler` is called with the halt reason.
    /// If the halt_handler returns `Ok(Some(r))` the run loop will return `Ok(Some(r))`.
    /// If the halt_handler returns `Err(e)` the run loop will return `Err(e)` and a stack trace will be printed.
    /// If the halt_handler returns `Ok(None)` the run loop will continue running the core.
    /// If the call exceeds `timeout` the run loop will return `Err(anyhow!("Timeout"))`. TODO
    fn run<F, R>(
        &self,
        core: &mut Core,
        catch_hardfault: bool,
        catch_reset: bool,
        timeout: Option<Duration>,
        mut halt_handler: F,
    ) -> Result<Option<R>>
    where
        F: FnMut(HaltReason, &mut Core) -> Result<Option<R>>,
    {
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
            self.path.as_path(),
            &rtt_config,
            self.timestamp_offset,
        );

        let exit = Arc::new(AtomicBool::new(false));
        let sig_id = signal_hook::flag::register(signal::SIGINT, exit.clone())?;

        let mut stdout = std::io::stdout();
        let mut halt_reason = None;
        let mut timeouted = false;
        while !exit.load(Ordering::Relaxed) && halt_reason.is_none() {
            // check for halt first, poll rtt after.
            // this is important so we do one last poll after halt, so we flush all messages
            // the core printed before halting, such as a panic message.
            match core.status()? {
                probe_rs::CoreStatus::Halted(reason) => {
                    match halt_handler(reason, core) {
                        Ok(Some(r)) => halt_reason = Some(Ok(r)),
                        Err(e) => halt_reason = Some(Err(e)),
                        Ok(None) =>{
                            //TODO: auto respond properly to SYS_GET_CMDLINE in case it is not answered here!!
                            core.run()?;
                        }
                    }
                }
                probe_rs::CoreStatus::Running
                | probe_rs::CoreStatus::LockedUp
                | probe_rs::CoreStatus::Sleeping
                | probe_rs::CoreStatus::Unknown => {
                    // Carry on
                }
            }

            let had_rtt_data = poll_rtt(&mut rtta, core, &mut stdout)?;

            match timeout {
                Some(timeout) if start.elapsed() >= timeout => {
                    timeouted = true;
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

        let result = match halt_reason {
            None => {
                core.halt(Duration::from_secs(1))?;
                if timeouted {
                    Err(anyhow!("Timeout"))
                } else {
                    // manually halted with Control+C. Stop the core.
                    Ok(None)
                }
            }
            Some(reason) => match reason {
                Ok(r) => Ok(Some(r)),
                Err(e) => Err(e),
            },
        };

        if self.always_print_stacktrace || result.is_err() {
            print_stacktrace(core, self.path.as_path())?;
        }

        signal_hook::low_level::unregister(sig_id);
        signal_hook::flag::register_conditional_default(signal::SIGINT, exit)?;

        result
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
