mod common;
mod info;

use std::path::PathBuf;
use std::time::Instant;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::num::ParseIntError;

use memmap;

use ocd::{
    debug::debug::DebugInfo,
    probe::{
        debug_probe::{
            DebugProbeInfo,
        },
        stlink,
        daplink,
    },
    collection::{
        cores::CortexDump,
    },
    session::Session,
    memory::MI,
    target::{
        Target,
    },
};

use common::{
    with_device,
    with_dump,
    CliError,
};

use structopt::StructOpt;

use rustyline::Editor;

use capstone::{
    Capstone,
    Endian,
    prelude::*,
    arch::arm::ArchMode,
};

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
        /// The number associated with the debug probe to use
        n: usize,
        /// The target to be selected.
        target: Option<String>,
    },
    /// Resets the target attached to the selected debug probe
    #[structopt(name = "reset")]
    Reset {
        /// The number associated with the debug probe to use
        n: usize,
        /// The target to be selected.
        target: Option<String>,
        /// Whether the reset pin should be asserted or deasserted. If left open, just pulse it
        assert: Option<bool>,
    },
    #[structopt(name = "debug")]
    Debug {
        #[structopt(long, parse(from_os_str))]
        /// Dump file to debug
        dump: Option<PathBuf>,
        /// The target to be selected.
        target: Option<String>,
        #[structopt(long, parse(from_os_str))]
        /// Binary to debug
        exe: Option<PathBuf>,

        // The number associated with the probe to use
        n: usize,
    },
    /// Dump memory from attached target
    #[structopt(name = "dump")]
    Dump {
        /// The number associated with the debug probe to use
        n: usize,
        /// The target to be selected.
        target: Option<String>,
        /// The address of the memory to dump from the target (in hexadecimal without 0x prefix)
        #[structopt(parse(try_from_str = "parse_hex"))]
        loc: u32,
        /// The amount of memory (in words) to dump
        words: u32,
    },
    /// Download memory to attached target
    #[structopt(name = "d")]
    D {
        /// The number associated with the ST-Link to use
        n: usize,
        /// The target to be selected.
        target: Option<String>,
        /// The path to the file to be downloaded to the flash
        path: String,
    },
    #[structopt(name = "erase")]
    Erase {
        /// The number associated with the debug probe to use
        n: usize,
        /// The target to be selected.
        target: Option<String>,
        /// The address of the memory to dump from the target (in hexadecimal without 0x prefix)
        #[structopt(parse(try_from_str = "parse_hex"))]
        loc: u32
    },
    #[structopt(name = "trace")]
    Trace {
        /// The number associated with the debug probe to use
        n: usize,
        /// The target to be selected.
        target: Option<String>,
        /// The address of the memory to dump from the target (in hexadecimal without 0x prefix)
        #[structopt(parse(try_from_str = "parse_hex"))]
        loc: u32,
    },
}

fn main() {
    // Initialize the logging backend.
    pretty_env_logger::init();

    let matches = CLI::from_args();

    match matches {
        CLI::List {} => list_connected_devices(),
        CLI::Info { n, target } => crate::info::show_info_of_device(n, get_checked_target(target)).unwrap(),
        CLI::Reset { n, target, assert } => reset_target_of_device(n, get_checked_target(target), assert).unwrap(),
        CLI::Debug { n, target, exe, dump } => debug(n, get_checked_target(target), exe, dump).unwrap(),
        CLI::Dump { n, target, loc, words } => dump_memory(n, get_checked_target(target), loc, words).unwrap(),
        CLI::D { n, target, path } => download_program_fast(n, get_checked_target(target), path).unwrap(),
        CLI::Erase { n, target, loc } => erase_page(n, get_checked_target(target), loc).unwrap(),
        CLI::Trace { n, target, loc } => trace_u32_on_target(n, get_checked_target(target), loc).unwrap(),
    }
}

pub fn get_checked_target(name: Option<String>) -> Target {
    use colored::*;
    match ocd_targets::select_target(name) {
        Ok(target) => target,
        Err(ocd::target::TargetSelectionError::CouldNotAutodetect) => {
            eprintln!("    {} Target could not automatically be identified. Please specify one.", "Error".red().bold());
            std::process::exit(1);
        },
        Err(ocd::target::TargetSelectionError::TargetNotFound(name)) => {
            eprintln!("    {} Specified target ({}) was not found. Please select an existing one.", "Error".red().bold(), name);
            std::process::exit(1);
        },
        Err(ocd::target::TargetSelectionError::TargetCouldNotBeParsed(error)) => {
            eprintln!("    {} Target specification could not be parsed.", "Error".red().bold());
            eprintln!("    {} {}", "Error".red().bold(), error);
            std::process::exit(1);
        },
    }
}

fn list_connected_devices() {
    let links = get_connected_devices();

    if links.len() > 0 {
        println!("The following devices were found:");
        links
            .iter()
            .enumerate()
            .for_each(|(num, link)| println!( "[{}]: {:?}", num, link));
    } else {
        println!("No devices were found.");
    }
}

fn dump_memory(n: usize, target: Target, loc: u32, words: u32) -> Result<(), CliError> {
    with_device(n as usize, target, |mut session| {
        let mut data = vec![0 as u32; words as usize];

        // Start timer.
        let instant = Instant::now();

        // let loc = 220 * 1024;

        session.probe.read_block32(loc, &mut data.as_mut_slice())?;
        // Stop timer.
        let elapsed = instant.elapsed();

        // Print read values.
        for word in 0..words {
            println!("Addr 0x{:08x?}: 0x{:08x}", loc + 4 * word, data[word as usize]);
        }
        // Print stats.
        println!("Read {:?} words in {:?}", words, elapsed);

        Ok(())
    })
}

fn download_program_fast(n: usize, target: Target, path: String) -> Result<(), CliError> {
    with_device(n as usize, target, |mut session| {

        // Start timer.
        // let instant = Instant::now();

        let mm = session.target.memory_map.clone();
        let fd = ocd::probe::flash::download::FileDownloader::new();
        fd.download_file(
            &mut session,
            std::path::Path::new(&path.as_str()),
            ocd::probe::flash::download::Format::Elf,
            &mm
        ).unwrap();

        let r = Ok(());

        // Stop timer.
        // let elapsed = instant.elapsed();

        r
    })
}

#[allow(non_snake_case)]
fn erase_page(n: usize, target: Target, loc: u32) -> Result<(), CliError> {
    with_device(n, target, |mut session| {

        // TODO: Generic flash erase

        let NVMC = 0x4001E000;
        let NVMC_CONFIG = NVMC + 0x504;
        let NVMC_ERASEPAGE = NVMC + 0x508;
        let EEN: u32 = 0x2;

        session.probe.write32(NVMC_CONFIG, EEN)?;
        session.probe.write32(NVMC_ERASEPAGE, loc)?;

        Ok(())
    })
}

fn reset_target_of_device(n: usize, target: Target, _assert: Option<bool>) -> Result<(), CliError> {
    with_device(n as usize, target, |mut session| {
        //link.get_interface_mut::<DebugProbe>().unwrap().target_reset().or_else(|e| Err(Error::DebugProbe(e)))?;
        session.probe.target_reset()?;

        Ok(())
    })
}

fn trace_u32_on_target(n: usize, target: Target, loc: u32) -> Result<(), CliError> {
    use std::io::prelude::*;
    use std::thread::sleep;
    use std::time::Duration;
    use scroll::{Pwrite};

    let mut xs = vec![];
    let mut ys = vec![];

    let start = Instant::now();

    with_device(n, target, |mut session| {
        loop {
            // Prepare read.
            let elapsed = start.elapsed();
            let instant = elapsed.as_secs() * 1000 + u64::from(elapsed.subsec_millis());

            // Read data.
            let value: u32 = session.probe.read32(loc)?;

            xs.push(instant);
            ys.push(value);

            // Send value to plot.py.
            // Unwrap is safe as there is always an stdin in our case!
            let mut buf = [0 as u8; 8];
            // Unwrap is safe!
            buf.pwrite(instant, 0).unwrap();
            buf.pwrite(value, 4).unwrap();
            std::io::stdout().write(&buf)?;

            std::io::stdout() .flush()?;

            // Schedule next read.
            let elapsed = start.elapsed();
            let instant = elapsed.as_secs() * 1000 + u64::from(elapsed.subsec_millis());
            let poll_every_ms = 50;
            let time_to_wait = poll_every_ms - instant % poll_every_ms;
            sleep(Duration::from_millis(time_to_wait));
        }
    })
}

fn get_connected_devices() -> Vec<DebugProbeInfo>{
    let mut links = daplink::tools::list_daplink_devices();
    links.extend(stlink::tools::list_stlink_devices());
    links
}

fn debug(n: usize, target: Target, exe: Option<PathBuf>, dump: Option<PathBuf>) -> Result<(), CliError> {
    
    // try to load debug information
    let debug_data = exe.and_then(|p| fs::File::open(&p).ok() )
                        .and_then(|file| unsafe { memmap::Mmap::map(&file).ok() });


    //let file = fs::File::open(&path).unwrap();
    //let mmap = Rc::new(Box::new(unsafe { memmap::Mmap::map(&file).unwrap() }));

    
    let runner = |mut session| {
        let mut cs = Capstone::new()
            .arm()
            .mode(ArchMode::Thumb)
            .endian(Endian::Little)
            .build()
            .unwrap();



        let di = debug_data.as_ref().map( |mmap| DebugInfo::from_raw(&*mmap));
        
        /*
        if let Some(ref path) = exe {

            DebugInfo::from_file(path)
        } else {
            DebugInfo::none()
        }; */

        let mut rl = Editor::<()>::new();
        //rl.set_auto_add_history(true);

        loop {
            let readline = rl.readline(">> ");
            match readline {
                Ok(line) => {
                    let history_entry: &str = line.as_ref();
                    rl.add_history_entry(history_entry);
                    handle_line(&mut session, &mut cs, di.as_ref(), &line)?;
                },
                Err(e) => {
                    // Just quit for now
                    println!("Error handling input: {:?}", e);
                    return Ok(());
                }
            }
        }
    };

    match dump {
        None => with_device(n, target, &runner),
        Some(p) => with_dump(&p, target, &runner),
    }

}


fn handle_line(session: &mut Session, cs: &mut Capstone, debug_info: Option<&DebugInfo>, line: &str) -> Result<(), CliError> {
    let mut command_parts = line.split_whitespace();

    let command = command_parts.next().unwrap();

    match command {
        "halt" => {
            let cpu_info = session.target.core.halt(&mut session.probe)?;
            println!("Core stopped at address 0x{:08x}", cpu_info.pc);

            let mut code = [0u8;16*2];

            session.probe.read_block8(cpu_info.pc, &mut code)?;


            let instructions = cs.disasm_all(&code, cpu_info.pc as u64).unwrap();

            for i in instructions.iter() {
                println!("{}", i);
            }


            Ok(())
        },
        "run" => {
            session.target.core.run(&mut session.probe)?;
            Ok(())
        },
        "step" => {
            let cpu_info = session.target.core.step(&mut session.probe)?;
            println!("Core stopped at address 0x{:08x}", cpu_info.pc);
            Ok(())
        },
        "read" => {
            let address_str = command_parts.next().unwrap();
            let address = u32::from_str_radix(address_str, 16).unwrap();
            //println!("Would read from address 0x{:08x}", address);

            let val = session.probe.read32(address)?;
            println!("0x{:08x} = 0x{:08x}", address, val);
            Ok(())
        },
        "break" => {
            let address_str = command_parts.next().unwrap();
            let address = u32::from_str_radix(address_str, 16).unwrap();
            //println!("Would read from address 0x{:08x}", address);

            session.target.core.enable_breakpoints(&mut session.probe, true)?;
            session.target.core.set_breakpoint(&mut session.probe, address)?;

            Ok(())
        },
        "bt" => {
            let regs = session.target.core.registers();
            let program_counter = session.target.core.read_core_reg(&mut session.probe, regs.PC)?;


            if let Some(di) = debug_info {
                let frames = di.try_unwind(session, program_counter as u64);

                for frame in frames {
                    println!("{}", frame);
                }
            }


            Ok(())
        },
        "dump" => {
            // dump all relevant data, stack and regs for now..
            //
            // stack beginning -> assume beginning to be hardcoded


            let stack_top: u32 = 0x2000_0000 + 0x4_000;

            let regs = session.target.core.registers();

            let stack_bot: u32 = session.target.core.read_core_reg(&mut session.probe, regs.SP)?;
            let pc: u32 = session.target.core.read_core_reg(&mut session.probe, regs.PC)?;
            
            let mut stack = vec![0u8;(stack_top - stack_bot) as usize];

            session.probe.read_block8(stack_bot, &mut stack[..])?;

            let mut dump = CortexDump::new(stack_bot, stack);

            for i in 0..12 {
                dump.regs[i as usize] = session.target.core.read_core_reg(&mut session.probe, i.into())?;
            }

            dump.regs[13] = stack_bot;
            dump.regs[14] = session.target.core.read_core_reg(&mut session.probe, regs.LR)?;
            dump.regs[15] = pc;

            let serialized = ron::ser::to_string(&dump).expect("Failed to serialize dump");

            let mut dump_file = File::create("dump.txt").expect("Failed to create file");

            dump_file.write_all(serialized.as_bytes()).expect("Failed to write dump file");


            Ok(())
        },
        "quit" => {
            Err(CliError::Quit)
        },
        _ => {
            println!("Unknown command '{}'", line);
            Ok(())
        }
    }
}
