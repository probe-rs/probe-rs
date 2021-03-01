use anyhow::{anyhow, Result};
use colored::*;
use std::sync::Mutex;
use std::{
    process::{self},
    time::Duration,
};
use structopt::StructOpt;

use probe_rs::{config::TargetSelector, DebugProbeInfo, DebugProbeSelector, Probe};

#[derive(Debug, StructOpt)]
struct Opt {
    #[structopt(name = "chip", long = "chip")]
    chip: Option<String>,
    #[structopt(
        name = "chip description file path",
        short = "c",
        long = "chip-description-path"
    )]
    chip_description_path: Option<String>,
    #[structopt(
        name = "reset-halt",
        long = "reset-halt",
        help = "Use this flag to reset and halt (instead of just a halt) the attached core after attaching to the target."
    )]
    reset_halt: bool,
    #[structopt(
        name = "gdb-connection-string",
        long = "gdb-connection-string",
        help = "Use this flag to override the default GDB connection string (localhost:1337)."
    )]
    gdb_connection_string: Option<String>,
    #[structopt(
        name = "list-probes",
        long = "list-probes",
        help = "list available debug probes"
    )]
    list: bool,
    #[structopt(
        name = "debug probe index",
        long = "probe-index",
        short = "n",
        help = "select index of debug probe to use"
    )]
    probe_index: Option<usize>,
    #[structopt(
        long = "probe",
        help = "Use this flag to select a specific probe in the list by vendor and product id.\n\
        Use '--probe VID:PID' or '--probe VID:PID:Serial' if you have more than one probe with the same VID:PID.\n\
        If there are multiple probes with the same VID:PID:Serial, you have to specify it with '--probe-index'."
    )]
    probe_selector: Option<DebugProbeSelector>,
}

fn main() {
    pretty_env_logger::init();
    match main_try() {
        Ok(_) => (),
        Err(e) => {
            eprintln!("{}: {}", "error".red().bold(), e);
            process::exit(1);
        }
    }
}

pub fn open_probe(index: Option<usize>, available_probes: &[DebugProbeInfo]) -> Result<Probe> {
    let device = match index {
        Some(index) => available_probes
            .get(index)
            .ok_or_else(|| anyhow!("Unable to open the specified probe. Use the '--list-probes' flag to see all available probes."))?,
        None => {
            // open the default probe, if only one probe was found
            match available_probes.len() {
                1 => &available_probes[0],
                0 => return Err(anyhow!("No probe found.")),
                _ => return Err(anyhow!("Multiple probes found. Please specify which probe to use using the -n option.")),
            }
        }
    };

    let probe = Probe::open(device)?;

    Ok(probe)
}

fn main_try() -> Result<()> {
    // Get commandline options.
    let opt = Opt::from_iter(std::env::args());

    let mut available_probes = Probe::list_all();

    // Only retain probes with matching probe selector
    if let Some(selector) = opt.probe_selector {
        available_probes.retain(|probe| {
            probe.vendor_id == selector.vendor_id
                && probe.product_id == selector.product_id
                && if let Some(serial) = &selector.serial_number {
                    probe.serial_number.as_ref() == Some(serial)
                } else {
                    true
                }
        });
    }

    if opt.list {
        println!("Available probes:");
        for (idx, probe) in available_probes.iter().enumerate() {
            println!("[{}]: {:?}", idx, probe);
        }
        return Ok(());
    }

    let probe = open_probe(opt.probe_index, &available_probes)?;

    let target_selector = match opt.chip {
        Some(identifier) => identifier.into(),
        None => TargetSelector::Auto,
    };

    let session = Mutex::new(probe.attach(target_selector)?);

    if opt.reset_halt {
        session
            .lock()
            .unwrap()
            .core(0)?
            .reset_and_halt(Duration::from_millis(100))?;
    }

    let gdb_connection_string = opt
        .gdb_connection_string
        .or_else(|| Some("localhost:1337".to_string()));
    // This next unwrap will always resolve as the connection string is always Some(T).
    println!(
        "Firing up GDB stub at {}",
        gdb_connection_string.as_ref().unwrap()
    );
    if let Err(e) = probe_rs_gdb_server::run(gdb_connection_string, &session) {
        eprintln!("During the execution of GDB an error was encountered:");
        eprintln!("{:?}", e);
    }

    Ok(())
}
