use std::cell::RefCell;
use std::fs::File;
use std::io::Write;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use libtest_mimic::{Failed, Trial};
use probe_rs::debug::{DebugInfo, DebugRegisters};
use probe_rs::flashing::{FileDownloadError, Format};
use probe_rs::{
    exception_handler_for_core, BreakpointCause, Core, CoreInterface, CoreStatus, Error,
    HaltReason, Lister, MemoryInterface, SemihostingCommand, Session, VectorCatchCondition,
};
use probe_rs_target::MemoryRegion;
use signal_hook::consts::signal;
use static_cell::StaticCell;
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

    /// <elf> and the remaining arguments for the test runner (list tests, filter tests etc). Run `probe-rs test -- --help` for more information.
    #[clap(last(true))]
    pub(crate) libtest_args: Vec<String>,
}

static SESSION: StaticCell<Session> = StaticCell::new();
static RUNNER: StaticCell<RefCell<Runner>> = StaticCell::new();
struct Runner {
    core: Core<'static>,
    timestamp_offset: UtcOffset,
    path: String,
    always_print_stacktrace: bool,
    no_location: bool,
    log_format: Option<String>,
    memory_map: Vec<MemoryRegion>,
    rtt_scan_regions: Vec<Range<u64>>,
}

const SEMIHOSTING_USER_LIST: u32 = 0x100;

impl Cmd {
    pub fn run(
        self,
        lister: &Lister,
        run_download: bool,
        timestamp_offset: UtcOffset,
    ) -> Result<()> {
        let path = self.libtest_args[0].clone();
        let libtest_args = libtest_mimic::Arguments::from_iter(self.libtest_args);

        let (mut session, probe_options) = self.probe_options.simple_attach(lister)?;

        if run_download {
            let mut file = match File::open(&path) {
                Ok(file) => file,
                Err(e) => {
                    return Err(FileDownloadError::IO(e)).context("Failed to open binary file.")
                }
            };

            let mut loader = session.target().flash_loader();

            let format = self.format_options.into_format(session.target())?;
            match format {
                Format::Bin(options) => loader.load_bin_data(&mut file, options),
                Format::Elf => loader.load_elf_data(&mut file),
                Format::Hex => loader.load_hex_data(&mut file),
                Format::Idf(options) => loader.load_idf_data(&mut session, &mut file, options),
                Format::Uf2 => loader.load_uf2_data(&mut file),
            }?;
            let path = Path::new(&path);
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
        drop(core);

        let session = SESSION.init(session);
        let runner = RUNNER.init(RefCell::new(Runner {
            core: session.core(0)?,
            timestamp_offset,
            path: path.to_owned(),
            always_print_stacktrace: self.always_print_stacktrace,
            no_location: self.no_location,
            log_format: self.log_format.clone(),
            memory_map,
            rtt_scan_regions,
        }));

        let tests = create_tests(runner)?;
        libtest_mimic::run(&libtest_args, tests).exit()
    }
}

const SYS_EXIT_EXTENDED: u32 = 0x20;

fn run_until_semihosting(core: &mut Core) -> Result<SemihostingCommand> {
    core.run()?;

    loop {
        match core.status()? {
            CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(
                SemihostingCommand::Unknown { operation, .. },
            ))) if operation == SYS_EXIT_EXTENDED => {
                tracing::debug!("Got SYS_EXIT_EXTENDED. Continuing");
                core.run()?;
            }
            CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(s))) => {
                tracing::debug!("Got semihosting command from target {:?}", s);
                return Ok(s);
            }
            CoreStatus::Halted(r) => bail!("core halted {:?}", r),
            probe_rs::CoreStatus::Running
            | probe_rs::CoreStatus::LockedUp
            | probe_rs::CoreStatus::Sleeping
            | probe_rs::CoreStatus::Unknown => {}
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn run_until_exact_semihosting(core: &mut Core, operation: u32) -> Result<u32> {
    match run_until_semihosting(core)? {
        SemihostingCommand::ExitSuccess | SemihostingCommand::ExitError { .. } => {
            bail!("Unexpected exit of target at program start")
        }
        SemihostingCommand::Unknown {
            operation: op,
            parameter,
        } => {
            if op == operation {
                Ok(parameter)
            } else {
                bail!("Unexpected semihosting operation: {:x}", operation)
            }
        }
    }
}

struct Buffer {
    address: u32,
    len: u32,
}

impl Buffer {
    fn from_block_at(core: &mut Core, block_addr: u32) -> Result<Self> {
        let mut block: [u32; 2] = [0, 0];
        core.read_32(block_addr as u64, &mut block)?;
        Ok(Self {
            address: block[0],
            len: block[1],
        })
    }

    fn read(&mut self, core: &mut Core) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; self.len as usize];
        core.read(self.address as u64, &mut buf[..])?;
        Ok(buf)
    }

    // Writes the passed buffer to the target. The buffer must end with \0
    // length written will not include \0.
    fn write_to_block_at(&mut self, core: &mut Core, block_addr: u32, buf: &[u8]) -> Result<()> {
        if buf.len() > self.len as usize {
            bail!("buffer not large enough")
        }
        if *buf.last().unwrap() != 0 {
            bail!("last byte is not 0");
        }
        core.write_8(self.address as u64, buf)?;
        let block: [u32; 2] = [self.address, (buf.len() - 1) as u32];
        core.write_32(block_addr as u64, &block)?;
        Ok(())
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

/// Asks the target for the tests, and create closures to run the tests later
fn create_tests(runner_ref: &'static RefCell<Runner>) -> Result<Vec<Trial>> {
    let mut runner = runner_ref.borrow_mut();
    let core = &mut runner.core;
    // Run target with arg "list", so that it lists all tests
    {
        const SYS_GET_CMDLINE: u32 = 0x15;
        let block_address = run_until_exact_semihosting(core, SYS_GET_CMDLINE)?;
        let mut buf = Buffer::from_block_at(core, block_address)?;
        buf.write_to_block_at(core, block_address, b"list\0")?;

        let reg = core.registers().get_argument_register(0).unwrap();
        core.write_core_reg(reg, 0u32)?; // write status = success
    }

    // Wait until the target calls the user defined Semihosting Operation and reports the tests
    {
        let block_address = run_until_exact_semihosting(core, SEMIHOSTING_USER_LIST)?;
        let mut buf = Buffer::from_block_at(core, block_address)?;
        let buf = buf.read(core)?;

        let list: Tests = serde_json::from_slice(&buf[..])?;
        tracing::debug!("got list of tests from target: {:?}", list);
        if list.version != 1 {
            bail!("Unsupported test list format version: {}", list.version);
        }

        let mut tests = Vec::<Trial>::new();
        for t in &list.tests {
            let test = t.clone();
            tests.push(
                Trial::test(&t.name, move || {
                    let mut runner = runner_ref.borrow_mut();
                    run_test(test, &mut *runner)
                })
                .with_ignored_flag(t.ignored),
            )
        }
        Ok(tests)
    }
}

// Run a single test on the target
fn run_test(test: Test, runner: &mut Runner) -> std::result::Result<(), Failed> {
    let core = &mut runner.core;
    tracing::info!("Running test {}", test.name);
    core.reset_and_halt(Duration::from_millis(100))?;

    // Run target with arg "run <testname>"
    {
        const SYS_GET_CMDLINE: u32 = 0x15;
        let block_address = run_until_exact_semihosting(core, SYS_GET_CMDLINE)?;
        let mut buf = Buffer::from_block_at(core, block_address)?;
        let cmd = format!("run {}\0", test.name).into_bytes();
        buf.write_to_block_at(core, block_address, &cmd)?;
        let reg = core.registers().get_argument_register(0).unwrap();
        core.write_core_reg(reg, 0u32)?; // write status = success
    }

    let timeout = test.timeout.map(|t| Duration::from_secs(t as u64));
    let timeout = timeout.unwrap_or(Duration::from_secs(60)); // TODO: make global timeout configurable

    // Wait on semihosting abort/exit
    match run_loop(
        core,
        &runner.memory_map,
        &runner.rtt_scan_regions,
        &runner.path,
        runner.timestamp_offset,
        runner.always_print_stacktrace,
        runner.no_location,
        runner.log_format.as_deref(),
        test.should_panic,
        timeout,
    ) {
        Ok(o) => match o {
            Ok(_) => {
                tracing::info!("Test {} passed", test.name);
                Ok(())
            }
            Err(e) => {
                tracing::info!("Test {} failed: {:?}", test.name, e);
                Err(e)
            }
        },
        Err(e) => {
            eprintln!("Error: {:?}", e);
            std::process::exit(1);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_loop(
    core: &mut Core<'_>,
    memory_map: &[MemoryRegion],
    rtt_scan_regions: &[Range<u64>],
    path: &str,
    timestamp_offset: UtcOffset,
    always_print_stacktrace: bool,
    no_location: bool,
    log_format: Option<&str>,
    should_panic: bool,
    timeout: Duration,
) -> Result<std::result::Result<(), Failed>, anyhow::Error> {
    let mut rtt_config = rtt::RttConfig::default();
    rtt_config.channels.push(rtt::RttChannelConfig {
        channel_number: Some(0),
        show_location: !no_location,
        ..Default::default()
    });
    let path = Path::new(path);

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
    let start = Instant::now();

    core.run()?;

    let mut stderr = std::io::stderr();
    let mut halt_reason = None;
    let mut timeouted = false;
    while !exit.load(Ordering::Relaxed) && halt_reason.is_none() {
        // check for halt first, poll rtt after.
        // this is important so we do one last poll after halt, so we flush all messages
        // the core printed before halting, such as a panic message.
        match core.status()? {
            probe_rs::CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(
                SemihostingCommand::Unknown { operation, .. },
            ))) if operation == SYS_EXIT_EXTENDED => {
                tracing::info!("Target wanted to run semihosting SYS_EXIT_EXTENDED (0x20), but probe-rs does not support this operation yet. Continuing...");
                core.run()?;
            }
            probe_rs::CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Semihosting(
                SemihostingCommand::Unknown { operation, .. },
            ))) if operation != SEMIHOSTING_USER_LIST => {
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

        if start.elapsed() >= timeout {
            timeouted = true;
            break;
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

            if always_print_stacktrace {
                print_stacktrace(core, path)?;
            }

            if timeouted {
                Ok(Err(Failed::from(format!(
                    "Test timed out after {:?}",
                    timeout
                ))))
            } else {
                // manually halted with Control+C. Stop the core.
                Err(anyhow!("CPU halted by user."))
            }
        }
        Some(reason) => {
            let exit_status =
                match reason {
                    HaltReason::Breakpoint(BreakpointCause::Semihosting(s)) => {
                        match s
                        {
                            SemihostingCommand::ExitSuccess => Ok(true),
                            SemihostingCommand::ExitError { .. } => Ok(false),
                            SemihostingCommand::Unknown { operation, parameter } => Err(Failed::from(format!("Expected the target to run the test and exit/error with semihosting. Instead it requested semihosting operation: {} {:x}", operation, parameter)))
                        }
                    },
                    _ => Err(Failed::from("CPU halted unexpectedly.")),
                };

            match exit_status {
                Err(e) => Ok(Err(e)),
                Ok(exit_status) => {
                    if exit_status == !should_panic {
                        if always_print_stacktrace && !exit_status {
                            print_stacktrace(core, path)?;
                        }
                        Ok(Ok(()))
                    } else {
                        if !exit_status {
                            print_stacktrace(core, path)?;
                        }
                        Ok(Err(Failed::from(format!(
                            "Test should have {} but it {}",
                            if should_panic { "panicked" } else { "passed" },
                            if exit_status { "passed" } else { "panicked" }
                        ))))
                    }
                }
            }
        }
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
        log::error!("No debug info found.");
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
    stderr: &mut std::io::Stderr,
) -> Result<bool, anyhow::Error> {
    let mut had_data = false;
    if let Some(rtta) = rtta {
        for (_ch, data) in rtta.poll_rtt_fallible(core)? {
            if !data.is_empty() {
                had_data = true;
            }
            stderr.write_all(data.as_bytes())?;
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
    for _ in 0..RTT_RETRIES {
        match rtt::attach_to_rtt(
            core,
            memory_map,
            scan_regions,
            path,
            &rtt_config,
            timestamp_offset,
            log_format,
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
