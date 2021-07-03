use std::{path::Path, time::Duration};

use crate::{
    dut_definition::{DefinitionSource, DutDefinition},
    tests::{
        stepping::test_stepping, test_flashing, test_hw_breakpoints, test_memory_access,
        test_register_access,
    },
};
use anyhow::{bail, Context, Result};

use clap::{App, Arg};

mod dut_definition;
mod tests;

fn main() -> Result<()> {
    pretty_env_logger::init();

    let app = App::new("smoke tester")
        .arg(
            Arg::with_name("dut_definitions")
                .long("dut-definitions")
                .value_name("DIRECTORY")
                .takes_value(true)
                .conflicts_with_all(&["chip", "probe", "single_dut"])
                .required(true),
        )
        .arg(
            Arg::with_name("chip")
                .long("chip")
                .takes_value(true)
                .value_name("CHIP")
                .conflicts_with_all(&["dut_definitions", "single_dut"])
                .required(true),
        )
        .arg(
            Arg::with_name("probe")
                .long("probe")
                .takes_value(true)
                .value_name("PROBE")
                .required(false),
        )
        .arg(
            Arg::with_name("single_dut")
                .long("single-dut")
                .value_name("FILE")
                .takes_value(true)
                .required(true)
                .conflicts_with_all(&["chip", "dut_definitions"]),
        );

    let matches = app.get_matches();

    let definitions = if let Some(dut_definitions) = matches.value_of("dut_definitions") {
        let definitions = DutDefinition::collect(dut_definitions)?;
        println!("Found {} target definitions.", definitions.len());
        definitions
    } else if let Some(single_dut) = matches.value_of("single_dut") {
        vec![DutDefinition::from_file(Path::new(single_dut))?]
    } else {
        // Chip needs to be specified
        let chip = matches.value_of("chip").unwrap(); // If dut-definitions is not present, chip must be present

        if let Some(probe) = matches.value_of("probe") {
            vec![DutDefinition::new(&chip, &probe)?]
        } else {
            vec![DutDefinition::autodetect_probe(&chip)?]
        }
    };

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

        let target = session.target();
        let core_name = target.cores[core_index].name.clone();

        let mut core = session.core(core_index)?;

        println!("Halting core..");

        core.reset_and_halt(Duration::from_millis(500))?;

        test_register_access(&mut core)?;

        test_memory_access(&mut core, &core_name, &memory_regions)?;

        test_hw_breakpoints(&mut core, &memory_regions)?;

        test_stepping(&mut core, &memory_regions)?;

        // Ensure core is not running anymore.
        core.reset_and_halt(Duration::from_millis(200))?;
    }

    if let Some(flash_binary) = &definition.flash_test_binary {
        test_flashing(&mut session, flash_binary)?;
    }

    drop(session);

    // Try attaching with hard reset

    if definition.reset_connected {
        let probe = definition.open_probe()?;

        let _session = probe.attach_under_reset(definition.chip.clone())?;
    }

    Ok(())
}
