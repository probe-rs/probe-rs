use std::{
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

use crate::dut_definition::{DefinitionSource, DutDefinition};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use linkme::distributed_slice;
use probe_rs::{Permissions, probe::WireProtocol};

mod dut_definition;
mod macros;
mod tests;

#[derive(Debug, thiserror::Error)]
pub enum TestFailure {
    #[error("Test returned an error")]
    Error(#[from] anyhow::Error),

    #[error("The test was skipped: {0}")]
    Skipped(String),

    #[error("Test is not implemented for target {0:?}: {1}")]
    UnimplementedForTarget(Box<probe_rs::Target>, String),
    #[error("A resource necessary to execute the test is not available: {0}")]
    MissingResource(String),

    /// A fatal error means that all future tests will be cancelled as well.
    #[error("A fatal error occured")]
    Fatal(#[source] anyhow::Error),
}

impl From<probe_rs::Error> for TestFailure {
    fn from(error: probe_rs::Error) -> Self {
        TestFailure::Error(error.into())
    }
}

pub type TestResult = Result<(), TestFailure>;

#[derive(Debug)]
struct SingleTestReport {
    result: TestResult,
    _duration: Duration,
}

impl SingleTestReport {
    fn failed(&self) -> bool {
        matches!(
            self.result,
            Err(TestFailure::Error(_) | TestFailure::Fatal(_))
        )
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    Test {
        #[arg(long, value_name = "FILE")]
        markdown_summary: Option<PathBuf>,
    },
}

#[derive(Debug, Parser)]
struct Opt {
    #[command(subcommand)]
    command: Command,

    #[arg(long, global = true, value_name = "DIRECTORY", conflicts_with_all = ["chip", "probe", "single_dut"])]
    dut_definitions: Option<PathBuf>,

    #[arg(long, global = true, value_name = "CHIP", conflicts_with_all = ["dut_definitions", "single_dut"])]
    chip: Option<String>,

    #[arg(long, global = true, value_name = "PROBE")]
    probe: Option<String>,

    #[arg(long, global = true, value_name = "PROBE_SPEED")]
    probe_speed: Option<u32>,

    #[arg(long, global = true, value_name = "PROTOCOL")]
    protocol: Option<WireProtocol>,

    #[arg(long, global = true, value_name = "FILE", conflicts_with_all = ["chip", "dut_definitions"])]
    single_dut: Option<PathBuf>,
}

fn main() -> Result<ExitCode> {
    env_logger::init();

    let opt = Opt::parse();

    let mut definitions = if let Some(dut_definitions) = opt.dut_definitions.as_deref() {
        let definitions = DutDefinition::collect(dut_definitions)?;
        println!("Found {} target definitions.", definitions.len());
        definitions
    } else if let Some(single_dut) = opt.single_dut.as_deref() {
        vec![DutDefinition::from_file(Path::new(single_dut))?]
    } else {
        // Chip needs to be specified
        let chip = opt.chip.as_deref().unwrap(); // If dut-definitions is not present, chip must be present

        if let Some(probe) = &opt.probe {
            vec![DutDefinition::new(chip, probe)?]
        } else {
            vec![DutDefinition::autodetect_probe(chip)?]
        }
    };

    for definition in &mut definitions {
        if let Some(probe_speed) = opt.probe_speed {
            definition.probe_speed = Some(probe_speed);
        }

        if let Some(protcol) = opt.protocol {
            definition.protocol = Some(protcol);
        }
    }

    match opt.command {
        Command::Test { markdown_summary } => run_test(&definitions, markdown_summary),
    }
}

fn run_test(definitions: &[DutDefinition], markdown_summary: Option<PathBuf>) -> Result<ExitCode> {
    let mut test_tracker = TestTracker::new(definitions);

    let result = test_tracker.run(|tracker, definition| {
        let probe = definition.open_probe()?;

        println_dut_status!(tracker, blue, "Probe: {:?}", probe.get_name());
        println_dut_status!(tracker, blue, "Chip:  {:?}", &definition.chip.name);

        // We don't care about existing flash contents
        let permissions = Permissions::default().allow_erase_all();

        let mut fail_counter = 0;

        let mut session = probe
            .attach(definition.chip.clone(), permissions)
            .context("Failed to attach to chip")?;
        let cores = session.list_cores();

        // TODO: Handle different cores. Handling multiple cores is not supported properly yet,
        //       some cores need additional setup so that they can be used, and this is not handled yet.
        for (core_index, core_type) in cores.into_iter().take(1) {
            println_dut_status!(tracker, blue, "Core {}: {:?}", core_index, core_type);

            let mut core = session.core(core_index)?;

            println_dut_status!(tracker, blue, "Halting core..");

            core.reset_and_halt(Duration::from_millis(500))?;

            for test_fn in CORE_TESTS {
                let result = tracker.run_test(|tracker| test_fn(tracker, &mut core));

                if let Err(TestFailure::Fatal(error)) = result.result {
                    return Err(error.context("Fatal error in test"));
                }

                if result.failed() {
                    fail_counter += 1;
                }
            }

            // Ensure core is not running anymore.
            core.reset_and_halt(Duration::from_millis(200))
                .with_context(|| {
                    format!("Failed to reset core with index {core_index} after test")
                })?;
        }

        for test in SESSION_TESTS {
            let result = tracker.run_test(|tracker| test(tracker, &mut session));

            if let Err(TestFailure::Fatal(error)) = result.result {
                return Err(error.context("Fatal error in test"));
            }

            if result.failed() {
                fail_counter += 1;
            }
        }

        drop(session);

        // Try attaching with hard reset

        if definition.reset_connected {
            let probe = definition.open_probe()?;

            let _session =
                probe.attach_under_reset(definition.chip.clone(), Permissions::default())?;
        }

        match fail_counter {
            0 => Ok(()),
            1 => anyhow::bail!("1 test failed"),
            count => anyhow::bail!("{count} tests failed"),
        }
    });

    println!();

    let printer = ConsoleReportPrinter;

    printer.print(&result, std::io::stdout())?;

    if let Some(summary_file) = &markdown_summary {
        let mut file = std::fs::File::create(summary_file).with_context(|| {
            format!(
                "Failed to create markdown summary file at location {}",
                summary_file.display()
            )
        })?;

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

#[derive(Debug)]
pub struct TestTracker<'dut> {
    dut_definitions: &'dut [DutDefinition],
    current_dut: usize,
    current_test: usize,
}

impl<'dut> TestTracker<'dut> {
    fn new(dut_definitions: &'dut [DutDefinition]) -> Self {
        Self {
            dut_definitions,
            current_dut: 0,
            current_test: 0,
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

    fn advance_test(&mut self) {
        self.current_test += 1;
    }

    pub fn current_target(&self) -> &probe_rs::Target {
        &self.dut_definitions[self.current_dut].chip
    }

    pub fn current_dut_definition(&self) -> &DutDefinition {
        &self.dut_definitions[self.current_dut]
    }

    #[must_use]
    fn run(
        &mut self,
        handle_dut: impl Fn(&mut TestTracker, &DutDefinition) -> anyhow::Result<()> + Sync + Send,
    ) -> TestReport {
        let mut report = TestReport::new();

        let mut tests_ok = true;

        for definition in self.dut_definitions {
            print_dut_status!(self, blue, "Starting Test");

            if let DefinitionSource::File(path) = &definition.source {
                print!(" - {}", path.display());
            }
            println!();

            let join_result =
                std::thread::scope(|s| s.spawn(|| handle_dut(self, definition)).join());

            match join_result {
                Ok(Ok(())) => {
                    report.add_report(DutReport {
                        name: definition.chip.name.clone(),
                        succesful: true,
                    });
                    println_dut_status!(self, green, "Tests Passed");
                }
                Ok(Err(e)) => {
                    tests_ok = false;
                    report.add_report(DutReport {
                        name: definition.chip.name.clone(),
                        succesful: false,
                    });

                    println_dut_status!(self, red, "Error message: {:#}", e);

                    if let Some(source) = e.source() {
                        println_dut_status!(self, red, " caused by:    {}", source);
                    }

                    println_dut_status!(self, red, "Tests Failed");
                }
                Err(_join_err) => {
                    tests_ok = false;
                    report.add_report(DutReport {
                        name: definition.chip.name.clone(),
                        succesful: false,
                    });

                    println_dut_status!(self, red, "Panic while running tests.");
                }
            }

            self.advance_dut();
        }

        if tests_ok {
            println_status!(self, green, "All DUTs passed.");
        } else {
            println_status!(self, red, "Some DUTs failed some tests.");
        }

        report
    }

    fn run_test(
        &mut self,
        test: impl FnOnce(&TestTracker) -> Result<(), TestFailure>,
    ) -> SingleTestReport {
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
            Err(TestFailure::UnimplementedForTarget(target, message)) => {
                println_test_status!(
                    self,
                    yellow,
                    "Test not implemented for {}: {}",
                    target.name,
                    message
                );
            }
            Err(TestFailure::MissingResource(message)) => {
                println_test_status!(self, yellow, "Missing resource for test: {}", message);
            }
            Err(_e) => {
                println_test_status!(self, red, "{_e:?}");
                println_test_status!(self, red, "Test failed in {formatted_duration}.");
            }
        };
        self.advance_test();

        SingleTestReport {
            result: test_result,
            _duration: duration,
        }
    }
}

/// A list of all tests which run on cores.
#[distributed_slice]
pub static CORE_TESTS: [fn(&TestTracker, &mut probe_rs::Core) -> TestResult];

/// A list of all tests which run on `Session`.
#[distributed_slice]
pub static SESSION_TESTS: [fn(&TestTracker, &mut probe_rs::Session) -> TestResult];
