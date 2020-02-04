use structopt;

use colored::*;
use std::{
    path::Path,
    process::{self},
    sync::{Arc, Mutex},
};
use structopt::StructOpt;

use probe_rs::{
    config::registry::{Registry, SelectionStrategy},
    Probe,
};

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
        help = "Use this flag to reset and halt (instead of just a reset) the attached core after flashing the target."
    )]
    reset_halt: bool,
    #[structopt(
        name = "gdb-connection-string",
        long = "gdb-connection-string",
        help = "Use this flag to override the default GDB connection string (localhost:1337)."
    )]
    gdb_connection_string: Option<String>,
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

pub fn open_probe(index: Option<usize>) -> Result<Probe, failure::Error> {
    let available_probes = Probe::list_all();

    let device = match index {
        Some(index) => available_probes
            .get(index)
            .ok_or(failure::err_msg("Unable to open the specified probe. Use the 'list' subcommand to see all available probes."))?,
        None => {
            // open the default probe, if only one probe was found
            if available_probes.len() == 1 {
                &available_probes[0]
            } else {
                return Err(failure::err_msg("Multiple probes found. Please specify which probe to use using the -n parameter."));
            }
        }
    };

    let probe = Probe::from_probe_info(&device)?;

    Ok(probe)
}

fn main_try() -> Result<(), failure::Error> {
    // Get commandline options.
    let opt = Opt::from_iter(std::env::args());

    let probe = open_probe(None)?;

    let strategy = if let Some(identifier) = opt.chip.clone() {
        SelectionStrategy::TargetIdentifier(identifier.into())
    } else {
        eprintln!("Autodetection of the target is currently disabled for stability reasons.");
        std::process::exit(1);
        // TODO:
        // SelectionStrategy::ChipInfo(ChipInfo::read_from_rom_table(&mut probe)?)
    };

    let mut registry = Registry::from_builtin_families();
    if let Some(cdp) = opt.chip_description_path {
        registry.add_target_from_yaml(&Path::new(&cdp))?;
    }

    let target = registry.get_target(strategy)?;
    let session = probe.attach(target, None)?;

    let gdb_connection_string = opt
        .gdb_connection_string
        .or_else(|| Some("localhost:1337".to_string()));
    // This next unwrap will always resolve as the connection string is always Some(T).
    println!(
        "Firing up GDB stub at {}",
        gdb_connection_string.as_ref().unwrap()
    );
    if let Err(e) = probe_rs_gdb_server::run(gdb_connection_string, Arc::new(Mutex::new(session))) {
        eprintln!("During the execution of GDB an error was encountered:");
        eprintln!("{:?}", e);
    }

    Ok(())
}
