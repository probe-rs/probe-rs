use std::{path::PathBuf, process::ExitCode, time::Duration};

use crate::dut_definition::DutDefinition;

use anyhow::{Context, Result};
use clap::Parser;
use libtest_mimic::{Arguments, Failed, Trial};
use linkme::distributed_slice;
use probe_rs::Permissions;
use tracing_subscriber::EnvFilter;

mod dut_definition;
mod macros;
mod tests;

pub type TestResult = Result<(), Failed>;

#[derive(Debug, Parser)]
#[clap(name = "smoke-tester")]
struct Opt {
    #[arg(value_name = "DIRECTORY", env = "SMOKE_TESTER_CONFIG")]
    dut_definitions: PathBuf,

    #[clap(flatten)]
    test: Arguments,
}

fn main() -> Result<ExitCode> {
    // nextest will get angry if logs are emitted outside of tests,
    // so we use a separate ENV variable here, so setting RUST_LOG doesn't affect this
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_env("SMOKE_TESTER_LOG"))
        .init();

    let (test_args, dut_definition) = if std::env::var("NEXTEST").is_ok() {
        let args = Arguments::from_args();
        let Some(dut_definition) = std::env::var_os("SMOKE_TESTER_CONFIG") else {
            anyhow::bail!("SMOKE_TESTER_CONFIG environment variable not set");
        };

        (args, PathBuf::from(dut_definition))
    } else {
        let test_args = Opt::parse();

        (test_args.test, test_args.dut_definitions)
    };

    let definitions = if dut_definition.is_file() {
        vec![DutDefinition::from_file(&dut_definition)?]
    } else {
        DutDefinition::collect(&dut_definition)?
    };
    //println!("Found {} target definitions.", definitions.len());

    run_test(test_args, &definitions)
}

fn run_test(mut args: Arguments, definitions: &[DutDefinition]) -> Result<ExitCode> {
    let mut trials = Vec::new();

    for definition in definitions.iter() {
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
                    Err(err) => Err(err),
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

                    result
                })
                .with_kind(&chip_name);

                trials.push(cores_trial);
            }
        }
    }

    // Ensure tests are not run in parallel
    args.test_threads = Some(1);

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_env_var("SMOKE_TESTER_TEST_LOG")
                .with_default_directive(tracing::level_filters::LevelFilter::TRACE.into())
                .from_env_lossy(),
        )
        .finish();

    let conclusion =
        tracing::subscriber::with_default(subscriber, || libtest_mimic::run(&args, trials));

    Ok(conclusion.exit_code())
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
