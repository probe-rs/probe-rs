use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use crate::dut_definition::DutDefinition;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use libtest_mimic::{Arguments, Failed, Trial};
use linkme::distributed_slice;
use probe_rs::{Permissions, probe::WireProtocol};

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

    let args = std::env::args().collect::<Vec<_>>();

    let delimiter = args.iter().rposition(|arg| arg == "--");

    let (test_args, other_args) = if let Some(delimiter) = delimiter {
        let (test_args, other_args) = args.split_at(delimiter);
        (test_args, &other_args[1..])
    } else {
        (args.as_slice(), &[] as _)
    };

    // TODO: Handle arguments
    let test_args = Arguments::from_args();

    // Require dut definition as an environment variable
    let Some(dut_definition) = std::env::var_os("SMOKE_TESTER_CONFIG") else {
        anyhow::bail!("SMOKE_TESTER_CONFIG environment variable not set");
    };

    let definitions = DutDefinition::collect(Path::new(&dut_definition))?;
    println!("Found {} target definitions.", definitions.len());

    run_test(test_args, &definitions)
}

fn run_test(mut args: Arguments, definitions: &[DutDefinition]) -> Result<ExitCode> {
    let mut trials = Vec::new();

    for (index, definition) in definitions.iter().enumerate() {
        // Log some information
        let probe = definition.open_probe()?;

        println!("DUT {}", index + 1);
        println!(" Probe: {:?}", probe.get_name());
        println!(" Chip:  {:?}", &definition.chip.name);

        let chip_name = definition.chip.name.clone();

        for (i, test) in SESSION_TESTS.iter().enumerate() {
            let session_definition = definition.clone();

            let trial = Trial::test(format!("Session test {i}"), move || {
                let probe = session_definition.open_probe()?;

                // We don't care about existing flash contents
                let permissions = Permissions::default().allow_erase_all();

                let mut session = probe
                    .attach(session_definition.chip.clone(), permissions)
                    .context("Failed to attach to chip")?;

                match test(&session_definition, &mut session) {
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
            for test_fn in CORE_TESTS {
                let definition_for_cores = definition.clone();
                let cores_trial = Trial::test("Cores", move || {
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

                    let result = test_fn(&definition, &mut core);

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

/// A list of all tests which run on cores.
#[distributed_slice]
pub static CORE_TESTS: [fn(&DutDefinition, &mut probe_rs::Core) -> TestResult];

/// A list of all tests which run on `Session`.
#[distributed_slice]
pub static SESSION_TESTS: [fn(&DutDefinition, &mut probe_rs::Session) -> TestResult];
