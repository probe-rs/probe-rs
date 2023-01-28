use std::{marker::PhantomData, path::Path, time::Duration};

use crate::{
    dut_definition::{DefinitionSource, DutDefinition},
    tests::{
        stepping::test_stepping, test_flashing, test_hw_breakpoints, test_memory_access,
        test_register_access,
    },
};
use anyhow::{Context, Result};
use colored::Colorize;

use clap::{Arg, Command};
use probe_rs::{Error, Permissions};

mod dut_definition;
mod macros;
mod tests;

fn main() -> Result<()> {
    pretty_env_logger::init();

    let app = Command::new("smoke tester")
        .arg(
            Arg::new("dut_definitions")
                .long("dut-definitions")
                .value_name("DIRECTORY")
                .conflicts_with_all(["chip", "probe", "single_dut"])
                .required(true),
        )
        .arg(
            Arg::new("chip")
                .long("chip")
                .value_name("CHIP")
                .conflicts_with_all(["dut_definitions", "single_dut"])
                .required(true),
        )
        .arg(
            Arg::new("probe")
                .long("probe")
                .value_name("PROBE")
                .required(false),
        )
        .arg(
            Arg::new("single_dut")
                .long("single-dut")
                .value_name("FILE")
                .required(true)
                .conflicts_with_all(["chip", "dut_definitions"]),
        );

    let matches = app.get_matches();

    let definitions = if let Some(dut_definitions) = matches.get_one::<String>("dut_definitions") {
        let definitions = DutDefinition::collect(dut_definitions)?;
        println!("Found {} target definitions.", definitions.len());
        definitions
    } else if let Some(single_dut) = matches.get_one::<String>("single_dut") {
        vec![DutDefinition::from_file(Path::new(single_dut))?]
    } else {
        // Chip needs to be specified
        let chip = matches.get_one::<String>("chip").unwrap(); // If dut-definitions is not present, chip must be present

        if let Some(probe) = matches.get_one::<String>("probe") {
            vec![DutDefinition::new(chip, probe)?]
        } else {
            vec![DutDefinition::autodetect_probe(chip)?]
        }
    };

    let mut test_tracker = TestTracker::new(definitions, 0);

    test_tracker.run(|tracker, definition| {
        let probe = definition.open_probe()?;

        println_dut_status!(tracker, blue, "Probe: {:?}", probe.get_name());
        println_dut_status!(tracker, blue, "Chip:  {:?}", &definition.chip.name);

        let mut session = probe
            .attach(definition.chip.clone(), Permissions::default())
            .context("Failed to attach to chip")?;
        let target = session.target();
        let memory_regions = target.memory_map.clone();
        let cores = session.list_cores();

        for (core_index, core_type) in cores {
            println_dut_status!(tracker, blue, "Core {}: {:?}", core_index, core_type);

            let target = session.target();
            let core_name = target.cores[core_index].name.clone();

            let mut core = session.core(core_index)?;

            println_dut_status!(tracker, blue, "Halting core..");

            core.reset_and_halt(Duration::from_millis(500))?;

            tracker.run_test(|tracker| {
                test_register_access(tracker, &mut core)?;
                Ok(())
            })?;

            tracker.run_test(|tracker| {
                test_memory_access(tracker, &mut core, &core_name, &memory_regions)?;
                Ok(())
            })?;

            tracker.run_test(|tracker| {
                test_hw_breakpoints(tracker, &mut core, &memory_regions)?;
                Ok(())
            })?;

            tracker.run_test(|_tracker| {
                test_stepping(&mut core, &memory_regions)?;
                Ok(())
            })?;

            // Ensure core is not running anymore.
            core.reset_and_halt(Duration::from_millis(200))?;
        }

        if let Some(flash_binary) = &definition.flash_test_binary {
            tracker.run_test(|tracker| {
                test_flashing(tracker, &mut session, flash_binary)?;
                Ok(())
            })?;
        }

        drop(session);

        // Try attaching with hard reset

        if definition.reset_connected {
            let probe = definition.open_probe()?;

            let _session =
                probe.attach_under_reset(definition.chip.clone(), Permissions::default())?;
        }

        Ok(())
    });

    Ok(())
}
pub struct TestTracker<'a> {
    dut_definitions: Vec<DutDefinition>,
    current_dut: usize,
    num_tests: usize,
    current_test: usize,
    _marker: PhantomData<&'a ()>,
}

impl<'a> TestTracker<'a> {
    fn new(dut_definitions: Vec<DutDefinition>, num_tests: usize) -> Self {
        Self {
            dut_definitions,
            current_dut: 0,
            num_tests,
            current_test: 0,
            _marker: PhantomData,
        }
    }

    fn advance_dut(&mut self) {
        self.current_dut += 1;
        self.current_test = 0;
    }

    fn current_dut(&self) -> usize {
        self.current_dut + 1
    }

    fn current_dut_name(&self) -> &str {
        &self.dut_definitions[self.current_dut].chip.name
    }

    fn num_duts(&self) -> usize {
        self.dut_definitions.len()
    }

    fn current_test(&self) -> usize {
        self.current_test + 1
    }

    fn num_tests(&self) -> usize {
        self.num_tests
    }

    fn advance_test(&mut self) {
        self.current_test += 1;
    }

    fn run(&mut self, handle_dut: impl Fn(&mut TestTracker, &DutDefinition) -> Result<(), Error>) {
        let mut tests_ok = true;
        for definition in &mut self.dut_definitions.clone() {
            print_dut_status!(self, blue, "Starting Test",);

            if let DefinitionSource::File(path) = &definition.source {
                print!(" - {}", path.display());
            }
            println!();

            match handle_dut(self, definition) {
                Ok(()) => {
                    println_dut_status!(self, green, "Tests Passed",);
                }
                Err(e) => {
                    tests_ok = false;

                    println_dut_status!(self, red, "Error message: {:#}", e);
                    println_dut_status!(self, red, "Tests Failed",);
                }
            }

            self.advance_dut();
        }

        if tests_ok {
            println_status!(self, green, "All DUTs passed.",);
        } else {
            println_status!(self, red, "Some DUTs failed some tests.",);
        }
    }

    fn run_test(
        &mut self,
        test: impl FnOnce(&TestTracker) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let res = match test(self) {
            Ok(()) => {
                println_test_status!(self, green, "Test passed.");
                Ok(())
            }
            Err(e) => {
                println_test_status!(self, red, "Test failed.");
                Err(e)
            }
        };
        self.advance_test();
        res?;
        Ok(())
    }
}
