use std::{
    io::Write,
    marker::PhantomData,
    path::Path,
    process::ExitCode,
    time::{Duration, Instant},
};

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

fn main() -> Result<ExitCode> {
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
        )
        .arg(
            Arg::new("markdown_summary")
                .long("markdown-summary")
                .value_name("FILE")
                .required(false),
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

    let result = test_tracker.run(|tracker, definition| {
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

    println!();

    let printer = ConsoleReportPrinter;

    printer.print(&result, std::io::stdout())?;

    if let Some(summary_file) = matches.get_one::<String>("markdown_summary") {
        let mut file = std::fs::File::create(summary_file)?;

        writeln!(file, "## smoke-tester")?;

        for dut in &result.dut_tests {
            let test_state = if dut.succesful { "Passed" } else { "Failed" };

            writeln!(file, " - {}: {}", dut.name, test_state)?;
        }
    }

    Ok(result.exit_code())
}

#[derive(Debug)]
struct DutReport {
    name: String,
    succesful: bool,
}

#[derive(Debug)]
struct TestReport {
    dut_tests: Vec<DutReport>,
}

impl TestReport {
    fn new() -> Self {
        TestReport { dut_tests: vec![] }
    }

    fn add_report(&mut self, report: DutReport) {
        self.dut_tests.push(report)
    }

    fn any_failed(&self) -> bool {
        self.dut_tests.iter().any(|d| !d.succesful)
    }

    /// Return the appropriate exit code for the test result.
    ///
    /// This is current 0, or success, if all tests have passed,
    /// and the default failure exit code for the platform otherwise.
    fn exit_code(&self) -> ExitCode {
        if self.any_failed() {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }

    fn num_failed_tests(&self) -> usize {
        self.dut_tests.iter().filter(|d| !d.succesful).count()
    }

    fn num_tests(&self) -> usize {
        self.dut_tests.len()
    }
}

#[derive(Debug)]
struct ConsoleReportPrinter;

impl ConsoleReportPrinter {
    fn print(
        &self,
        report: &TestReport,
        mut writer: impl std::io::Write,
    ) -> Result<(), std::io::Error> {
        writeln!(writer, "Test summary:")?;

        for dut in &report.dut_tests {
            if dut.succesful {
                writeln!(writer, " [{}] passed", &dut.name)?;
            } else {
                writeln!(writer, " [{}] failed", &dut.name)?;
            }
        }

        // Write summary
        if report.any_failed() {
            writeln!(
                writer,
                "{} out of {} tests failed.",
                report.num_failed_tests(),
                report.num_tests()
            )?;
        } else {
            writeln!(writer, "All tests passed.")?;
        }

        Ok(())
    }
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

    #[must_use]
    fn run(
        &mut self,
        handle_dut: impl Fn(&mut TestTracker, &DutDefinition) -> Result<(), Error>,
    ) -> TestReport {
        let mut report = TestReport::new();

        let mut tests_ok = true;

        for definition in &self.dut_definitions.clone() {
            print_dut_status!(self, blue, "Starting Test",);

            if let DefinitionSource::File(path) = &definition.source {
                print!(" - {}", path.display());
            }
            println!();

            match handle_dut(self, definition) {
                Ok(()) => {
                    report.add_report(DutReport {
                        name: definition.chip.name.clone(),
                        succesful: true,
                    });
                    println_dut_status!(self, green, "Tests Passed",);
                }
                Err(e) => {
                    tests_ok = false;
                    report.add_report(DutReport {
                        name: definition.chip.name.clone(),
                        succesful: false,
                    });

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

        report
    }

    fn run_test(
        &mut self,
        test: impl FnOnce(&TestTracker) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let start_time = Instant::now();

        let test_result = test(self);

        let duration = start_time.elapsed();

        let formatted_duration = if duration < Duration::from_secs(1) {
            format!("{} ms", duration.as_millis())
        } else {
            format!("{:.2} s", duration.as_secs_f32())
        };

        match &test_result {
            Ok(()) => {
                println_test_status!(self, green, "Test passed in {formatted_duration}.");
            }
            Err(_e) => {
                println_test_status!(self, red, "Test failed in {formatted_duration}.");
            }
        };
        self.advance_test();

        test_result
    }
}
