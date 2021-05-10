use std::time::Duration;

use crate::{
    dut_definition::{DefinitionSource, DutDefinition},
    tests::{test_flashing, test_hw_breakpoints, test_memory_access, test_register_access},
};
use anyhow::{bail, Context, Result};

mod dut_definition;
mod tests;

use structopt::StructOpt;

#[derive(StructOpt)]
struct Options {
    path: String,
}

fn main() -> Result<()> {
    pretty_env_logger::init();

    let opts = Options::from_args();

    let definitions = DutDefinition::collect(&opts.path)?;

    println!("Found {} target definitions.", definitions.len());

    let num_duts = definitions.len();

    let mut tests_ok = true;

    for (i, definition) in definitions.iter().enumerate() {
        print!("DUT [{}/{}] - Starting test", i + 1, num_duts,);

        if let DefinitionSource::File(path) = &definition.source {
            print!(" - {}", path.display());
        }

        println!();

        match handle_dut(definition) {
            Ok(()) => {
                println!("DUT [{}/{}] - Tests Passed", i + 1, num_duts,);
            }
            Err(e) => {
                tests_ok = false;

                println!("DUT [{}/{}] - Error message: {:#}", i + 1, num_duts, e);
                println!("DUT [{}/{}] - Tests Failed", i + 1, num_duts,);
            }
        }
    }

    if tests_ok {
        Ok(())
    } else {
        bail!("Not all tests succesful");
    }
}

fn handle_dut(definition: &DutDefinition) -> Result<()> {
    let probe = definition.open_probe()?;

    println!("Probe: {:?}", probe.get_name());
    println!("Chip:  {:?}", &definition.chip.name);

    let mut session = probe
        .attach(definition.chip.clone())
        .context("Failed to attach to chip")?;

    let target = session.target();

    let memory_regions = target.memory_map.clone();

    let cores = session.list_cores();

    for (core_index, core_type) in cores {
        println!("Core {}: {:?}", core_index, core_type);

        let mut core = session.core(core_index)?;

        println!("Halting core..");

        core.reset_and_halt(Duration::from_millis(500))?;

        test_register_access(&mut core)?;

        test_memory_access(&mut core, &memory_regions)?;

        test_hw_breakpoints(&mut core, &memory_regions)?;

        // Ensure core is not running anymore.
        core.reset_and_halt(Duration::from_millis(200))?;
    }

    if let Some(flash_binary) = &definition.flash_test_binary {
        test_flashing(&mut session, flash_binary)?;
    }

    Ok(())
}
