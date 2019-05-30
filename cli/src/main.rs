mod common;
mod info;

use probe::debug_probe::MasterProbe;
use memory::{
    MI,
    flash_writer,
};
use std::time::Instant;

use probe::debug_probe::{
    DebugProbeInfo,
};

use common::{
    with_device,
    CliError,
};

use structopt::StructOpt;

use rustyline::Editor;

fn parse_hex(src: &str) -> Result<u32, std::num::ParseIntError> {
    u32::from_str_radix(src, 16)
}

#[derive(StructOpt)]
#[structopt(
    name = "ST-Link CLI",
    about = "Get info about the connected ST-Links",
    author = "Noah Hüsser <yatekii@yatekii.ch>"
)]
enum CLI {
    /// List all connected ST-Links
    #[structopt(name = "list")]
    List {},
    /// Gets infos about the selected ST-Link
    #[structopt(name = "info")]
    Info {
        /// The number associated with the ST-Link to use
        n: usize,
    },
    /// Resets the target attached to the selected ST-Link
    #[structopt(name = "reset")]
    Reset {
        /// The number associated with the ST-Link to use
        n: usize,
        /// Whether the reset pin should be asserted or deasserted. If left open, just pulse it
        assert: Option<bool>,
    },
    #[structopt(name = "debug")]
    Debug {
        // The number associated with the probe to use
        n: usize,
    },
    /// Dump memory from attached target
    #[structopt(name = "dump")]
    Dump {
        /// The number associated with the ST-Link to use
        n: usize,
        /// The address of the memory to dump from the target (in hexadecimal without 0x prefix)
        #[structopt(parse(try_from_str = "parse_hex"))]
        loc: u32,
        /// The amount of memory (in words) to dump
        words: u32,
    },
    /// Download memory to attached target
    #[structopt(name = "download")]
    Download {
        /// The number associated with the ST-Link to use
        n: usize,
        /// The path to the file to be downloaded to the flash
        path: String,
    },
    #[structopt(name = "erase")]
    Erase {
        /// The number associated with the ST-Link to use
        n: usize,
        /// The address of the memory to dump from the target (in hexadecimal without 0x prefix)
        #[structopt(parse(try_from_str = "parse_hex"))]
        loc: u32
    },
    #[structopt(name = "trace")]
    Trace {
        /// The number associated with the ST-Link to use
        n: usize,
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
        CLI::Info { n } => crate::info::show_info_of_device(n).unwrap(),
        CLI::Reset { n, assert } => reset_target_of_device(n, assert).unwrap(),
        CLI::Debug { n } => debug(n).unwrap(),
        CLI::Dump { n, loc, words } => dump_memory(n, loc, words).unwrap(),
        CLI::Download { n, path } => download_program(n, path).unwrap(),
        CLI::Erase { n, loc } => erase_page(n, loc).unwrap(),
        CLI::Trace { n, loc } => trace_u32_on_target(n, loc).unwrap(),
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

fn dump_memory(n: usize, loc: u32, words: u32) -> Result<(), CliError> {
    with_device(n as usize, |link| {
        let mut data = vec![0 as u32; words as usize];

        // Start timer.
        let instant = Instant::now();

        // let loc = 220 * 1024;

        link.read_block(loc, &mut data.as_mut_slice())?;
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

fn download_program(n: usize, path: String) -> Result<(), CliError> {
    with_device(n as usize, |mut link| {

        // Start timer.
        // let instant = Instant::now();

        // let NVMC = 0x4001E000;
        // let NVMC_CONFIG = NVMC + 0x504;
        // let WEN: u32 = 0x1;
        // let loc = 220 * 1024;

        // link.write(NVMC_CONFIG, WEN).or_else(|e| Err(Error::AccessPort(e)))?;
        // link.write(loc, 0x0u32).or_else(|e| Err(Error::AccessPort(e)))?;

        // // Stop timer.
        // let elapsed = instant.elapsed();

        flash_writer::download_hex(path, &mut link, 1024)?;

        Ok(())

        // Ok(())
    })
}

#[allow(non_snake_case)]
fn erase_page(n: usize, loc: u32) -> Result<(), CliError> {

    with_device(n, |link| {

        // TODO: Generic flash erase

        let NVMC = 0x4001E000;
        let NVMC_CONFIG = NVMC + 0x504;
        let NVMC_ERASEPAGE = NVMC + 0x508;
        let EEN: u32 = 0x2;

        link.write(NVMC_CONFIG, EEN)?;
        link.write(NVMC_ERASEPAGE, loc)?;

        Ok(())
    })
}

fn reset_target_of_device(n: usize, _assert: Option<bool>) -> Result<(), CliError> {
    with_device(n as usize, |link: &mut MasterProbe| {
        //link.get_interface_mut::<DebugProbe>().unwrap().target_reset().or_else(|e| Err(Error::DebugProbe(e)))?;
        link.target_reset()?;

        Ok(())
    })
}

fn trace_u32_on_target(n: usize, loc: u32) -> Result<(), CliError> {
    use std::io::prelude::*;
    use std::thread::sleep;
    use std::time::Duration;
    use scroll::{Pwrite};

    let mut xs = vec![];
    let mut ys = vec![];

    let start = Instant::now();

    with_device(n, |link| {
        loop {
            // Prepare read.
            let elapsed = start.elapsed();
            let instant = elapsed.as_secs() * 1000 + u64::from(elapsed.subsec_millis());

            // Read data.
            let value: u32 = link.read(loc)?;

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

fn debug(n: usize) -> Result<(), CliError> {
    with_device(n, |dev| {
        let mut rl = Editor::<()>::new();
        //rl.set_auto_add_history(true);

        loop {
            let readline = rl.readline(">> ");
            match readline {
                Ok(line) => {
                    let history_entry: &str = line.as_ref();
                    rl.add_history_entry(history_entry);
                    handle_line(dev, &line)?;
                },
                Err(e) => {
                    // Just quit for now
                    println!("Error handling input: {:?}", e);
                    return Ok(());
                }
            }
        }
    })
}

fn handle_line(dev: &mut MasterProbe, line: &str) -> Result<(), CliError> {
    match line {
        "halt" => {
            let cpu_info = dev.halt()?;
            println!("Core stopped at address 0x{:08x}", cpu_info.pc);
            Ok(())
        },
        "run" => {
            dev.run()?;
            Ok(())
        },
        "step" => {
            let cpu_info = dev.step()?;
            println!("Core stopped at address 0x{:08x}", cpu_info.pc);
            Ok(())
        },
        _ => {
            println!("Unknown command '{}'", line);
            Ok(())
        }
    }
}