mod benchmark;
mod cargo_embed;
mod cargo_flash;
mod common;
mod dap_server;
mod debugger;
mod gdb;
mod info;
mod run;
mod trace;
mod util;

include!(concat!(env!("OUT_DIR"), "/meta.rs"));

use anyhow::{Context, Result};
use byte_unit::Byte;
use clap::Parser;
use probe_rs::{
    architecture::arm::{component::TraceSink, swo::SwoConfig},
    debug::debug_info::DebugInfo,
    flashing::{erase_all, BinOptions, FileDownloadError, Format},
    MemoryInterface, Probe,
};
use rustyline::DefaultEditor;
use std::time::Instant;
use std::{ffi::OsString, fs::File, path::PathBuf};
use std::{num::ParseIntError, path::Path};
use time::{OffsetDateTime, UtcOffset};
use tracing::metadata::LevelFilter;
use tracing_subscriber::{
    fmt::format::FmtSpan, prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt,
    EnvFilter, Layer,
};

use crate::benchmark::{benchmark, BenchmarkOptions};
use crate::debugger::CliState;
use crate::util::{
    common_options::{CargoOptions, FlashOptions, ProbeOptions},
    flash::run_flash_download,
};

#[derive(clap::Parser)]
#[clap(
    name = "probe-rs",
    about = "The probe-rs CLI",
    version = meta::CARGO_VERSION,
    long_version = meta::LONG_VERSION
)]
struct Cli {
    /// Location for log file
    ///
    /// If no location is specified, the log file will be stored in a default directory.
    #[clap(long, global = true)]
    log_file: Option<PathBuf>,
    #[clap(subcommand)]
    subcommand: Subcommand,
}

#[derive(clap::Subcommand)]
enum Subcommand {
    /// Debug Adapter Protocol (DAP) server. See https://probe.rs/docs/tools/vscode/
    DapServer(dap_server::CliCommand),
    /// List all connected debug probes
    List {},
    /// Gets infos about the selected debug probe and connected target
    Info {
        #[clap(flatten)]
        common: ProbeOptions,
    },
    /// Resets the target attached to the selected debug probe
    Reset {
        #[clap(flatten)]
        shared: CoreOptions,

        #[clap(flatten)]
        common: ProbeOptions,

        /// Whether the reset pin should be asserted or deasserted. If left open, just pulse it
        assert: Option<bool>,
    },
    /// Run a GDB server
    Gdb {
        #[clap(
            long,
            help = "Use this flag to override the default GDB connection string (localhost:1337)."
        )]
        gdb_connection_string: Option<String>,

        #[clap(
            name = "reset-halt",
            long = "reset-halt",
            help = "Use this flag to reset and halt (instead of just a halt) the attached core after attaching to the target."
        )]
        reset_halt: bool,
        #[clap(flatten)]
        common: ProbeOptions,
    },
    /// Basic command line debugger
    Debug {
        #[clap(flatten)]
        shared: CoreOptions,

        #[clap(flatten)]
        common: ProbeOptions,

        #[clap(long, value_parser)]
        /// Binary to debug
        exe: Option<PathBuf>,
    },
    /// Dump memory from attached target
    Dump {
        #[clap(flatten)]
        shared: CoreOptions,

        #[clap(flatten)]
        common: ProbeOptions,

        /// The address of the memory to dump from the target.
        #[clap(value_parser = parse_u64)]
        loc: u64,
        /// The amount of memory (in words) to dump.
        #[clap(value_parser = parse_u32)]
        words: u32,
    },
    /// Download memory to attached target
    Download {
        #[clap(flatten)]
        common: ProbeOptions,

        /// Format of the file to be downloaded to the flash. Possible values are case-insensitive.
        #[clap(value_enum, ignore_case = true, default_value = "elf", long)]
        format: DownloadFileType,

        /// The address in memory where the binary will be put at. This is only considered when `bin` is selected as the format.
        #[clap(long, value_parser = parse_u64)]
        base_address: Option<u64>,
        /// The number of bytes to skip at the start of the binary file. This is only considered when `bin` is selected as the format.
        #[clap(long, value_parser = parse_u32)]
        skip_bytes: Option<u32>,

        /// The path to the file to be downloaded to the flash
        path: String,

        /// Whether to erase the entire chip before downloading
        #[clap(long)]
        chip_erase: bool,

        /// Whether to disable fancy progress reporting
        #[clap(long)]
        disable_progressbars: bool,

        /// Disable double-buffering when downloading flash.  If downloading times out, try this option.
        #[clap(long = "disable-double-buffering")]
        disable_double_buffering: bool,
    },
    /// Erase all nonvolatile memory of attached target
    Erase {
        #[clap(flatten)]
        common: ProbeOptions,
    },
    /// Flash and run an ELF program
    #[clap(name = "run")]
    Run {
        #[clap(flatten)]
        common: ProbeOptions,

        /// The path to the ELF file to flash and run
        path: String,

        /// Whether to erase the entire chip before downloading
        #[clap(long)]
        chip_erase: bool,

        /// Disable double-buffering when downloading flash.  If downloading times out, try this option.
        #[clap(long = "disable-double-buffering")]
        disable_double_buffering: bool,
    },
    /// Trace a memory location on the target
    #[clap(name = "trace")]
    Trace {
        #[clap(flatten)]
        shared: CoreOptions,

        #[clap(flatten)]
        common: ProbeOptions,

        /// The address of the memory to dump from the target.
        #[clap(value_parser = parse_u64)]
        loc: u64,
    },
    /// Configure and monitor ITM trace packets from the target.
    #[clap(name = "itm")]
    Itm {
        #[clap(flatten)]
        shared: CoreOptions,

        #[clap(flatten)]
        common: ProbeOptions,

        #[clap(value_parser = parse_u64)]
        duration_ms: u64,

        #[clap(subcommand)]
        source: ItmSource,
    },
    #[clap(subcommand)]
    Chip(Chip),
    Benchmark {
        #[clap(flatten)]
        common: ProbeOptions,

        #[clap(flatten)]
        options: BenchmarkOptions,
    },
}

#[derive(clap::Parser)]
/// Inspect internal registry of supported chips
enum Chip {
    /// Lists all the available families and their chips with their full.
    #[clap(name = "list")]
    List,
    /// Shows chip properties of a specific chip
    #[clap(name = "info")]
    Info {
        /// The name of the chip to display.
        name: String,
    },
}

/// Shared options for core selection, shared between commands
#[derive(clap::Parser)]
pub(crate) struct CoreOptions {
    #[clap(long, default_value = "0")]
    core: usize,
}

#[derive(clap::Subcommand)]
pub(crate) enum ItmSource {
    /// Direct ITM data to internal trace memory for extraction.
    /// Note: Not all targets support trace memory.
    #[clap(name = "memory")]
    TraceMemory,

    /// Direct ITM traffic out the TRACESWO pin for reception by the probe.
    #[clap(name = "swo")]
    Swo {
        /// The speed of the clock feeding the TPIU/SWO module in Hz.
        clk: u32,

        /// The desired baud rate of the SWO output.
        baud: u32,
    },
}

/// Determine the default location for the logfile
///
/// This has to be called as early as possible, and while the program
/// is single-threaded. Otherwise, determining the local time might fail.
fn default_logfile_location() -> Result<PathBuf> {
    let project_dirs = directories::ProjectDirs::from("rs", "probe-rs", "probe-rs")
        .context("the application storage directory could not be determined")?;
    let directory = project_dirs.data_dir();
    let logname = sanitize_filename::sanitize_with_options(
        format!(
            "{}.log",
            OffsetDateTime::now_local()?.unix_timestamp_nanos() / 1_000_000
        ),
        sanitize_filename::Options {
            replacement: "_",
            ..Default::default()
        },
    );
    std::fs::create_dir_all(directory).context(format!("{directory:?} could not be created"))?;

    let log_path = directory.join(logname);

    Ok(log_path)
}

fn multicall_check(args: &[OsString], want: &str) -> Option<Vec<OsString>> {
    let argv0 = Path::new(&args[0]);
    if let Some(command) = argv0.file_stem().and_then(|f| f.to_str()) {
        if command == want {
            return Some(args.to_vec());
        }
    }

    if let Some(command) = args.get(1).and_then(|f| f.to_str()) {
        if command == want {
            return Some(args[1..].to_vec());
        }
    }

    None
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if let Some(args) = multicall_check(&args, "cargo-flash") {
        cargo_flash::main(args);
        return Ok(());
    }
    if let Some(args) = multicall_check(&args, "cargo-embed") {
        cargo_embed::main(args);
        return Ok(());
    }

    let utc_offset = UtcOffset::current_local_offset()
        .context("Failed to determine local time for timestamps")?;

    // Parse the commandline options.
    let matches = Cli::parse_from(args);

    // the DAP server has special logging requirements. Run it before initializing logging,
    // so it can do its own special init.
    if let Subcommand::DapServer(cmd) = matches.subcommand {
        return dap_server::run(cmd, utc_offset);
    }

    let log_path = if let Some(location) = matches.log_file {
        location
    } else {
        default_logfile_location().context("Unable to determine default log file location.")?
    };

    let log_file = File::create(&log_path)?;

    let file_subscriber = tracing_subscriber::fmt::layer()
        .json()
        .with_file(true)
        .with_line_number(true)
        .with_span_events(FmtSpan::FULL)
        .with_writer(log_file);

    let stdout_subscriber = tracing_subscriber::fmt::layer()
        .compact()
        .without_time()
        .with_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::ERROR.into())
                .from_env_lossy(),
        );

    tracing_subscriber::registry()
        .with(stdout_subscriber)
        .with(file_subscriber)
        .init();

    tracing::info!("Writing log to {:?}", log_path);

    let result = match matches.subcommand {
        Subcommand::DapServer { .. } => unreachable!(), // handled above.
        Subcommand::List {} => list_connected_probes(),
        Subcommand::Info { common } => crate::info::show_info_of_device(&common),
        Subcommand::Gdb {
            gdb_connection_string,
            common,
            reset_halt,
        } => gdb::run_gdb_server(common, gdb_connection_string.as_deref(), reset_halt),
        Subcommand::Reset {
            shared,
            common,
            assert,
        } => reset_target_of_device(&shared, &common, assert),
        Subcommand::Debug {
            shared,
            common,
            exe,
        } => debug(&shared, &common, exe),
        Subcommand::Dump {
            shared,
            common,
            loc,
            words,
        } => dump_memory(&shared, &common, loc, words),
        Subcommand::Download {
            common,
            format,
            base_address,
            skip_bytes,
            path,
            chip_erase,
            disable_progressbars,
            disable_double_buffering,
        } => download_program_fast(
            common,
            format.into(base_address, skip_bytes),
            &path,
            chip_erase,
            disable_progressbars,
            disable_double_buffering,
        ),
        Subcommand::Run {
            common,
            path,
            chip_erase,
            disable_double_buffering,
        } => run::run(
            common,
            &path,
            chip_erase,
            disable_double_buffering,
            utc_offset,
        ),
        Subcommand::Erase { common } => erase(&common),
        Subcommand::Trace {
            shared,
            common,
            loc,
        } => trace_u32_on_target(&shared, &common, loc),
        Subcommand::Itm {
            shared,
            common,
            duration_ms,
            source,
        } => {
            let sink = match source {
                ItmSource::TraceMemory => TraceSink::TraceMemory,
                ItmSource::Swo { clk, baud } => TraceSink::Swo(SwoConfig::new(clk).set_baud(baud)),
            };
            trace::itm_trace(
                &shared,
                &common,
                sink,
                std::time::Duration::from_millis(duration_ms),
            )
        }
        Subcommand::Chip(Chip::List) => print_families().map_err(Into::into),
        Subcommand::Chip(Chip::Info { name }) => print_chip_info(name),
        Subcommand::Benchmark { common, options } => benchmark(common, options),
    };

    tracing::info!("Wrote log to {:?}", log_path);

    result
}

/// Lists all connected debug probes.
pub fn list_connected_probes() -> Result<()> {
    let probes = Probe::list_all();

    if !probes.is_empty() {
        println!("The following debug probes were found:");
        for (num, link) in probes.iter().enumerate() {
            println!("[{num}]: {link:?}");
        }
    } else {
        println!("No debug probes were found.");
    }
    Ok(())
}

/// Print all the available families and their contained chips to the
/// commandline.
pub fn print_families() -> Result<()> {
    println!("Available chips:");
    for family in probe_rs::config::families()? {
        println!("{}", &family.name);
        println!("    Variants:");
        for variant in family.variants() {
            println!("        {}", variant.name);
        }
    }
    Ok(())
}

/// Print all the available families and their contained chips to the
/// commandline.
pub fn print_chip_info(name: impl AsRef<str>) -> Result<()> {
    println!("{}", name.as_ref());
    let target = probe_rs::config::get_target_by_name(name)?;
    println!("Cores ({}):", target.cores.len());
    for core in target.cores {
        println!(
            "    - {} ({:?})",
            core.name.to_ascii_lowercase(),
            core.core_type
        );
    }

    fn get_range_len(range: &std::ops::Range<u64>) -> u64 {
        range.end - range.start
    }

    for memory in target.memory_map {
        match memory {
            probe_rs::config::MemoryRegion::Ram(region) => println!(
                "RAM: {:#010x?} ({})",
                &region.range,
                Byte::from_bytes(get_range_len(&region.range) as u128).get_appropriate_unit(true)
            ),
            probe_rs::config::MemoryRegion::Generic(region) => println!(
                "Generic: {:#010x?} ({})",
                &region.range,
                Byte::from_bytes(get_range_len(&region.range) as u128).get_appropriate_unit(true)
            ),
            probe_rs::config::MemoryRegion::Nvm(region) => println!(
                "NVM: {:#010x?} ({})",
                &region.range,
                Byte::from_bytes(get_range_len(&region.range) as u128).get_appropriate_unit(true)
            ),
        };
    }
    Ok(())
}

fn dump_memory(
    shared_options: &CoreOptions,
    common: &ProbeOptions,
    loc: u64,
    words: u32,
) -> Result<()> {
    let mut session = common.simple_attach()?;

    let mut data = vec![0_u32; words as usize];

    // Start timer.
    let instant = Instant::now();

    // let loc = 220 * 1024;

    let mut core = session.core(shared_options.core)?;

    core.read_32(loc, data.as_mut_slice())?;
    // Stop timer.
    let elapsed = instant.elapsed();

    // Print read values.
    for word in 0..words {
        println!(
            "Addr 0x{:08x?}: 0x{:08x}",
            loc + 4 * word as u64,
            data[word as usize]
        );
    }
    // Print stats.
    println!("Read {words:?} words in {elapsed:?}");

    Ok(())
}

fn download_program_fast(
    common: ProbeOptions,
    format: Format,
    path: &str,
    do_chip_erase: bool,
    disable_progressbars: bool,
    disable_double_buffering: bool,
) -> Result<()> {
    let mut session = common.simple_attach()?;

    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) => return Err(FileDownloadError::IO(e)).context("Failed to open binary file."),
    };

    let mut loader = session.target().flash_loader();

    match format {
        Format::Bin(options) => loader.load_bin_data(&mut file, options),
        Format::Elf => loader.load_elf_data(&mut file),
        Format::Hex => loader.load_hex_data(&mut file),
    }?;

    run_flash_download(
        &mut session,
        Path::new(path),
        &FlashOptions {
            disable_progressbars,
            disable_double_buffering,
            reset_halt: false,
            log: None,
            restore_unwritten: false,
            flash_layout_output_path: None,
            elf: None,
            work_dir: None,
            cargo_options: CargoOptions::default(),
            probe_options: common,
        },
        loader,
        do_chip_erase,
    )?;

    Ok(())
}

fn erase(common: &ProbeOptions) -> Result<()> {
    let mut session = common.simple_attach()?;

    erase_all(&mut session, None)?;

    Ok(())
}

fn reset_target_of_device(
    shared_options: &CoreOptions,
    common: &ProbeOptions,
    _assert: Option<bool>,
) -> Result<()> {
    let mut session = common.simple_attach()?;

    session.core(shared_options.core)?.reset()?;

    Ok(())
}

fn trace_u32_on_target(
    shared_options: &CoreOptions,
    common: &ProbeOptions,
    loc: u64,
) -> Result<()> {
    use scroll::{Pwrite, LE};
    use std::io::prelude::*;
    use std::thread::sleep;
    use std::time::Duration;

    let mut xs = vec![];
    let mut ys = vec![];

    let start = Instant::now();

    let mut session = common.simple_attach()?;

    let mut core = session.core(shared_options.core)?;

    loop {
        // Prepare read.
        let elapsed = start.elapsed();
        let instant = elapsed.as_secs() * 1000 + u64::from(elapsed.subsec_millis());

        // Read data.
        let value: u32 = core.read_word_32(loc)?;

        xs.push(instant);
        ys.push(value);

        // Send value to plot.py.
        let mut buf = [0_u8; 8];
        // Unwrap is safe!
        buf.pwrite_with(instant, 0, LE).unwrap();
        buf.pwrite_with(value, 4, LE).unwrap();
        std::io::stdout().write_all(&buf)?;

        std::io::stdout().flush()?;

        // Schedule next read.
        let elapsed = start.elapsed();
        let instant = elapsed.as_secs() * 1000 + u64::from(elapsed.subsec_millis());
        let poll_every_ms = 50;
        let time_to_wait = poll_every_ms - instant % poll_every_ms;
        sleep(Duration::from_millis(time_to_wait));
    }
}

fn debug(shared_options: &CoreOptions, common: &ProbeOptions, exe: Option<PathBuf>) -> Result<()> {
    let mut session = common.simple_attach()?;

    let di = exe
        .as_ref()
        .and_then(|path| DebugInfo::from_file(path).ok());

    let cli = debugger::DebugCli::new();

    let core = session.core(shared_options.core)?;

    let mut cli_data = debugger::CliData::new(core, di)?;

    let mut rl = DefaultEditor::new()?;

    loop {
        cli_data.print_state()?;

        let readline = rl.readline(">> ");
        match readline {
            Ok(line) => {
                let history_entry: &str = line.as_ref();
                rl.add_history_entry(history_entry)?;
                let cli_state = cli.handle_line(&line, &mut cli_data)?;

                match cli_state {
                    CliState::Continue => (),
                    CliState::Stop => break,
                }
            }
            Err(e) => {
                use rustyline::error::ReadlineError;

                match e {
                    // For end of file and ctrl-c, we just quit
                    ReadlineError::Eof | ReadlineError::Interrupted => return Ok(()),
                    actual_error => {
                        // Show error message and quit
                        println!("Error handling input: {actual_error:?}");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
enum DownloadFileType {
    Elf,
    Hex,
    Bin,
}

impl DownloadFileType {
    fn into(self, base_address: Option<u64>, skip: Option<u32>) -> Format {
        match self {
            DownloadFileType::Elf => Format::Elf,
            DownloadFileType::Hex => Format::Hex,
            DownloadFileType::Bin => Format::Bin(BinOptions {
                base_address,
                skip: skip.unwrap_or(0),
            }),
        }
    }
}

fn parse_u32(input: &str) -> Result<u32, ParseIntError> {
    parse_int::parse(input)
}

fn parse_u64(input: &str) -> Result<u64, ParseIntError> {
    parse_int::parse(input)
}
