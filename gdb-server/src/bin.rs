use colored::*;
use std::{
    process::{self},
    time::Duration,
};
use structopt::StructOpt;

use probe_rs::{config::TargetSelector, Probe};

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
        name = "halt",
        long = "halt",
        help = "Use this flag to halt core after attaching."
    )]
    halt: bool,
    #[structopt(
        name = "reset",
        long = "reset",
        help = "Use this flag to reset the core after attaching."
    )]
    reset: bool,
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
            .ok_or_else(|| failure::err_msg("Unable to open the specified probe. Use the 'list' subcommand to see all available probes."))?,
        None => {
            // open the default probe, if only one probe was found
            if available_probes.len() == 1 {
                &available_probes[0]
            } else {
                return Err(failure::err_msg("Multiple probes found. Please specify which probe to use using the -n parameter."));
            }
        }
    };

    let probe = Probe::open(device)?;

    Ok(probe)
}

fn main_try() -> Result<(), failure::Error> {
    // Get commandline options.
    let opt = Opt::from_iter(std::env::args());

    let probe = open_probe(None)?;

    let target_selector = match opt.chip {
        Some(identifier) => identifier.into(),
        None => TargetSelector::Auto,
    };
    let mut session = probe.attach(target_selector)?;

    if opt.reset {
        let mut core = session.core(0)?;
        if opt.halt {
            let halt_timeout = Duration::from_millis(500);
            core.reset_and_halt(halt_timeout)?;
        } else {
            core.reset()?;
        }
    } else if opt.halt {
        let mut core = session.core(0)?;
        let halt_timeout = Duration::from_millis(500);
        core.halt(halt_timeout)?;
    }

    let gdb_connection_string = opt
        .gdb_connection_string
        .or_else(|| Some("localhost:1337".to_string()));
    // This next unwrap will always resolve as the connection string is always Some(T).
    println!(
        "Firing up GDB stub at {}",
        gdb_connection_string.as_ref().unwrap()
    );
    if let Err(e) = probe_rs_gdb_server::run(gdb_connection_string, session) {
        eprintln!("During the execution of GDB an error was encountered:");
        eprintln!("{:?}", e);
    }

    Ok(())
}
