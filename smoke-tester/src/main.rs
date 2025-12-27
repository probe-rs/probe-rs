use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use crate::dut_definition::DutDefinition;

use anyhow::{Context, Result};
use clap::Parser;
use libtest_mimic::{Arguments, Failed, Trial};
use linkme::distributed_slice;
use probe_rs::Permissions;

mod dut_definition;
mod macros;
mod tests;

pub type TestResult = Result<(), Failed>;

#[derive(Debug, Parser)]
struct Opt {
    #[arg(long, value_name = "DIRECTORY")]
    dut_definitions: PathBuf,
}

fn main() -> Result<ExitCode> {
    tracing_subscriber::fmt::init();

    let test_args = Arguments::from_args();

    // Require dut definition as an environment variable
    let Some(dut_definition) = std::env::var_os("SMOKE_TESTER_CONFIG") else {
        anyhow::bail!("SMOKE_TESTER_CONFIG environment variable not set");
    };

    let definitions = DutDefinition::collect(Path::new(&dut_definition))?;
    //println!("Found {} target definitions.", definitions.len());

    run_test(test_args, &definitions)
}

fn run_test(mut args: Arguments, definitions: &[DutDefinition]) -> Result<ExitCode> {
    let mut trials = Vec::new();

    for (_index, definition) in definitions.iter().enumerate() {
        // Log some information
        //let probe = definition.open_probe()?;

        //println!("DUT {}", index + 1);
        //println!(" Probe: {:?}", probe.get_name());
        //println!(" Chip:  {:?}", &definition.chip.name);

        let chip_name = definition.chip.name.clone();

        for test in SESSION_TESTS {
            let session_definition = definition.clone();

            let trial = Trial::test(test.name, move || {
                let probe = session_definition.open_probe()?;

                // We don't care about existing flash contents
                let permissions = Permissions::default().allow_erase_all();

                let mut session = probe
                    .attach(session_definition.chip.clone(), permissions)
                    .context("Failed to attach to chip")?;

                match (test.test_fn)(&session_definition, &mut session) {
                    Ok(()) => Ok(()),
                    Err(err) => Err(err.into()),
                }
            })
            .with_kind(&chip_name);

            trials.push(trial);
        }

        // Try attaching with hard reset
        if definition.reset_connected {
            let definition = definition.clone();

            let trial = Trial::test("Hard Reset", move || {
                let probe = definition.open_probe()?;

                let _session =
                    probe.attach_under_reset(definition.chip.clone(), Permissions::default())?;
                Ok(())
            })
            .with_kind(&chip_name);

            trials.push(trial);
        }

        let cores: Vec<_> = definition
            .chip
            .cores
            .iter()
            .enumerate()
            .map(|(index, core)| (index, core.core_type))
            .collect();

        // TODO: Handle different cores. Handling multiple cores is not supported properly yet,
        //       some cores need additional setup so that they can be used, and this is not handled yet.
        for (core_index, core_type) in cores.into_iter().take(1) {
            for test_struct in CORE_TESTS {
                let definition_for_cores = definition.clone();
                let cores_trial = Trial::test(test_struct.name, move || {
                    let definition = definition_for_cores;

                    let probe = definition.open_probe()?;

                    // We don't care about existing flash contents
                    let permissions = Permissions::default().allow_erase_all();

                    let mut session = probe
                        .attach(definition.chip.clone(), permissions)
                        .context("Failed to attach to chip")?;

                    println!("Core {}: {:?}", core_index, core_type);

                    let mut core = session.core(core_index)?;

                    println!("Halting core..");

                    core.reset_and_halt(Duration::from_millis(500))?;

                    let result = (test_struct.test_fn)(&definition, &mut core);

                    // Ensure core is not running anymore.
                    core.reset_and_halt(Duration::from_millis(200))
                        .with_context(|| {
                            format!("Failed to reset core with index {core_index} after test")
                        })?;

                    result.map_err(|err| Failed::from(err))
                })
                .with_kind(&chip_name);

                trials.push(cores_trial);
            }
        }
    }

    /*
    let printer = ConsoleReportPrinter;
    if let Some(summary_file) = &markdown_summary {
        let mut file = std::fs::File::create(summary_file).with_context(|| {
            format!(
                "Failed to create markdown summary file at location {}",
                summary_file.display()
            )
        })?;

        writeln!(file, "## smoke-tester")?;

        for result in &reports {
            for dut in &result.dut_tests {
                let test_state = if dut.succesful { "Passed" } else { "Failed" };

                writeln!(file, " - {}: {}", dut.name, test_state)?;
            }
        }
    }


    for result in &reports {
        if result.any_failed() {
            return Ok(ExitCode::FAILURE);
        }
    }
    */

    // Ensure tests are not run in parallel
    args.test_threads = Some(1);

    Ok(libtest_mimic::run(&args, trials).exit_code())
}

struct NamedSessionTest {
    pub name: &'static str,
    pub test_fn: &'static fn(&DutDefinition, &mut probe_rs::Session) -> TestResult,
}

impl NamedSessionTest {
    const fn new(
        name: &'static str,
        test_fn: &'static fn(&DutDefinition, &mut probe_rs::Session) -> TestResult,
    ) -> Self {
        NamedSessionTest { name, test_fn }
    }
}

struct NamedCoreTest {
    pub name: &'static str,
    pub test_fn: &'static fn(&DutDefinition, &mut probe_rs::Core) -> TestResult,
}

impl NamedCoreTest {
    const fn new(
        name: &'static str,
        test_fn: &'static fn(&DutDefinition, &mut probe_rs::Core) -> TestResult,
    ) -> Self {
        NamedCoreTest { name, test_fn }
    }
}

/// A list of all tests which run on cores.
#[distributed_slice]
pub static CORE_TESTS: [NamedCoreTest];

/// A list of all tests which run on `Session`.
#[distributed_slice]
pub static SESSION_TESTS: [NamedSessionTest];
