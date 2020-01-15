use structopt;

use colored::*;
use failure::format_err;
use std::{
    path::Path,
    process::{self},
    sync::{Arc, Mutex},
};
use structopt::StructOpt;

use probe_rs::{
    config::registry::{Registry, SelectionStrategy},
    probe::{daplink, stlink, DebugProbe, DebugProbeType, MasterProbe, WireProtocol},
    session::Session,
    target::info::ChipInfo,
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

fn main_try() -> Result<(), failure::Error> {
    // Get commandline options.
    let opt = Opt::from_iter(std::env::args());

    let mut list = daplink::tools::list_daplink_devices();
    list.extend(stlink::tools::list_stlink_devices());

    let device = list
        .pop()
        .ok_or_else(|| format_err!("no supported probe was found"))?;

    let mut probe = match device.probe_type {
        DebugProbeType::DAPLink => {
            let mut link = daplink::DAPLink::new_from_probe_info(&device)?;

            link.attach(Some(WireProtocol::Swd))?;

            MasterProbe::from_specific_probe(link)
        }
        DebugProbeType::STLink => {
            let mut link = stlink::STLink::new_from_probe_info(&device)?;

            link.attach(Some(WireProtocol::Swd))?;

            MasterProbe::from_specific_probe(link)
        }
    };

    let strategy = if let Some(identifier) = opt.chip.clone() {
        SelectionStrategy::TargetIdentifier(identifier.into())
    } else {
        SelectionStrategy::ChipInfo(ChipInfo::read_from_rom_table(&mut probe)?)
    };

    let mut registry = Registry::from_builtin_families();
    if let Some(cdp) = opt.chip_description_path {
        registry.add_target_from_yaml(&Path::new(&cdp))?;
    }

    let target = registry.get_target(strategy)?;

    let session = Session::new(target, probe);

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
