mod common;
mod debugger;
mod info;

use common::with_device;
use debugger::CliState;

use probe_rs::{
    debug::DebugInfo,
    flashing::{download_file, Format},
    MemoryInterface, Probe, Session, WireProtocol,
};

use capstone::{arch::arm::ArchMode, prelude::*, Capstone, Endian};
use rustyline::Editor;
use structopt::StructOpt;

use anyhow::{anyhow, Result};

use std::num::ParseIntError;
use std::path::PathBuf;
use std::time::Instant;

fn parse_hex(src: &str) -> Result<u32, ParseIntError> {
    u32::from_str_radix(src, 16)
}

#[derive(StructOpt)]
#[structopt(
    name = "Probe-rs CLI",
    about = "A CLI for on top of the debug probe capabilities provided by probe-rs",
    author = "Noah Hüsser <yatekii@yatekii.ch> / Dominik Böhi <dominik.boehi@gmail.ch>"
)]
enum CLI {
    /// List all connected debug probes
    #[structopt(name = "list")]
    List {},
    /// Gets infos about the selected debug probe and connected target
    #[structopt(name = "info")]
    Info {
        #[structopt(flatten)]
        shared: SharedOptions,
    },
    /// Resets the target attached to the selected debug probe
    #[structopt(name = "reset")]
    Reset {
        #[structopt(flatten)]
        shared: SharedOptions,

        /// Whether the reset pin should be asserted or deasserted. If left open, just pulse it
        assert: Option<bool>,
    },
    #[structopt(name = "debug")]
    Debug {
        #[structopt(flatten)]
        shared: SharedOptions,

        #[structopt(long, parse(from_os_str))]
        /// Binary to debug
        exe: Option<PathBuf>,
    },
    /// Dump memory from attached target
    #[structopt(name = "dump")]
    Dump {
        #[structopt(flatten)]
        shared: SharedOptions,

        /// The address of the memory to dump from the target (in hexadecimal without 0x prefix)
        #[structopt(parse(try_from_str = parse_hex))]
        loc: u32,
        /// The amount of memory (in words) to dump
        words: u32,
    },
    /// Download memory to attached target
    #[structopt(name = "download")]
    Download {
        #[structopt(flatten)]
        shared: SharedOptions,

        /// The path to the file to be downloaded to the flash
        path: String,
    },
    #[structopt(name = "trace")]
    Trace {
        #[structopt(flatten)]
        shared: SharedOptions,

        /// The address of the memory to dump from the target (in hexadecimal without 0x prefix)
        #[structopt(parse(try_from_str = parse_hex))]
        loc: u32,
    },
}

/// Shared options for all commands which use a specific probe
#[derive(StructOpt)]
struct SharedOptions {
    /// The number associated with the debug probe to use
    #[structopt(long = "probe-index")]
    n: Option<usize>,

    /// The target to be selected.
    #[structopt(short, long)]
    chip: Option<String>,

    /// Protocol to use for target connection
    #[structopt(short, long)]
    protocol: Option<WireProtocol>,

    /// Protocol speed in kHz
    #[structopt(short, long)]
    speed: Option<u32>,

    #[structopt(long)]
    connect_under_reset: bool,
}

fn main() -> Result<()> {
    // Initialize the logging backend.
    pretty_env_logger::init();

    let matches = CLI::from_args();

    match matches {
        CLI::List {} => list_connected_devices(),
        CLI::Info { shared } => crate::info::show_info_of_device(&shared),
        CLI::Reset { shared, assert } => reset_target_of_device(&shared, assert),
        CLI::Debug { shared, exe } => debug(&shared, exe),
        CLI::Dump { shared, loc, words } => dump_memory(&shared, loc, words),
        CLI::Download { shared, path } => download_program_fast(&shared, &path),
        CLI::Trace { shared, loc } => trace_u32_on_target(&shared, loc),
    }
}

fn list_connected_devices() -> Result<()> {
    let links = Probe::list_all();

    if !links.is_empty() {
        println!("The following devices were found:");
        links
            .iter()
            .enumerate()
            .for_each(|(num, link)| println!("[{}]: {:?}", num, link));
    } else {
        println!("No devices were found.");
    }

    Ok(())
}

fn dump_memory(shared_options: &SharedOptions, loc: u32, words: u32) -> Result<()> {
    with_device(shared_options, |mut session| {
        let mut data = vec![0_u32; words as usize];

        // Start timer.
        let instant = Instant::now();

        // let loc = 220 * 1024;

        let mut core = session.core(0)?;

        core.read_32(loc, &mut data.as_mut_slice())?;
        // Stop timer.
        let elapsed = instant.elapsed();

        // Print read values.
        for word in 0..words {
            println!(
                "Addr 0x{:08x?}: 0x{:08x}",
                loc + 4 * word,
                data[word as usize]
            );
        }
        // Print stats.
        println!("Read {:?} words in {:?}", words, elapsed);

        Ok(())
    })
}

fn download_program_fast(shared_options: &SharedOptions, path: &str) -> Result<()> {
    with_device(shared_options, |mut session| {
        download_file(&mut session, std::path::Path::new(&path), Format::Elf)?;

        Ok(())
    })
}

fn reset_target_of_device(shared_options: &SharedOptions, _assert: Option<bool>) -> Result<()> {
    with_device(shared_options, |mut session| {
        session.core(0)?.reset()?;

        Ok(())
    })
}

fn trace_u32_on_target(shared_options: &SharedOptions, loc: u32) -> Result<()> {
    use scroll::{Pwrite, LE};
    use std::io::prelude::*;
    use std::thread::sleep;
    use std::time::Duration;

    let mut xs = vec![];
    let mut ys = vec![];

    let start = Instant::now();

    with_device(shared_options, |mut session| {
        let mut core = session.core(0)?;

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
    })
}

fn debug(shared_options: &SharedOptions, exe: Option<PathBuf>) -> Result<()> {
    let runner = |mut session: Session| {
        let cs = Capstone::new()
            .arm()
            .mode(ArchMode::Thumb)
            .endian(Endian::Little)
            .build()
            .map_err(|err| anyhow!("Error creating capstone: {:?}", err))?;

        let di = exe
            .as_ref()
            .and_then(|path| DebugInfo::from_file(path).ok());

        let cli = debugger::DebugCli::new();

        let core = session.core(0)?;

        let mut cli_data = debugger::CliData {
            core,
            debug_info: di,
            capstone: cs,
        };

        let mut rl = Editor::<()>::new();

        loop {
            let readline = rl.readline(">> ");
            match readline {
                Ok(line) => {
                    let history_entry: &str = line.as_ref();
                    rl.add_history_entry(history_entry);
                    let cli_state = cli.handle_line(&line, &mut cli_data)?;

                    match cli_state {
                        CliState::Continue => (),
                        CliState::Stop => return Ok(()),
                    }
                }
                Err(e) => {
                    use rustyline::error::ReadlineError;

                    match e {
                        // For end of file and ctrl-c, we just quit
                        ReadlineError::Eof | ReadlineError::Interrupted => return Ok(()),
                        actual_error => {
                            // Show error message and quit
                            println!("Error handling input: {:?}", actual_error);
                            return Ok(());
                        }
                    }
                }
            }
        }
    };

    with_device(shared_options, runner)
}
