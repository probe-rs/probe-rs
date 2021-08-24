use os_pipe::pipe;
use std::{
    io::Read,
    process::{Command, ExitStatus},
    sync::Mutex,
};
use structopt::lazy_static::lazy_static;

struct RunResult {
    exit_status: ExitStatus,
    output: String,
}

lazy_static! {
    /// rust will try to run the tests in parallel by default, and `insta` doesn't like the
    /// usual way of disabling this via `--test-threads=1`, so we're using this
    /// mutex to make sure we're not re-flashing until the last run is finished
    static ref ONE_RUN_AT_A_TIME: Mutex<i32> = Mutex::new(0i32);
}

/// run probe-run with `args` and truncate the "Finished .. in .." and "Running `...`" flashing output
/// NOTE: this currently only capures `stdin`, so any `log::` ed output, like flashing
fn run_and_truncate(args: &str) -> RunResult {
    let _guard = ONE_RUN_AT_A_TIME.lock().unwrap();

    // add prefix to run this repository's version of `probe-run` and
    // remove user-dependent registry and rustc information from backtrace paths
    let complete_command = format!("run -- {} --shorten-paths", args);

    let (mut reader, writer) = pipe().unwrap();
    let writer_clone = writer.try_clone().unwrap();

    let mut handle = Command::new("cargo")
        .args(complete_command.split(" "))
        // capture stderr and stdout while preserving line order
        .stdout(writer)
        .stderr(writer_clone)
        // run `probe-run`
        .spawn()
        .unwrap();

    // retrieve output and clean up
    let mut probe_run_output = String::new();
    reader.read_to_string(&mut probe_run_output).unwrap();
    let exit_status = handle.wait().unwrap();

    // remove the lines printed during flashing, as they contain timing info that's not always the same
    let output = probe_run_output
        .lines()
        .filter(|line| {
            !line.starts_with("    Finished")
            && !line.starts_with("     Running `")
            && !line.starts_with("    Blocking waiting for file lock ")
            && !line.starts_with("   Compiling probe-run v")
            // TODO don't drop the `└─ probe_run @ ...` locations after
            // https://github.com/knurling-rs/probe-run/issues/217 is resolved
            && !line.starts_with("└─ ")
        })
        .map(|line| format!("{}\n", line))
        .collect();

    RunResult {
        exit_status,
        output,
    }
}

#[test]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn successful_run_has_no_backtrace() {
    let run_result = run_and_truncate("--chip nRF52840_xxAA tests/test_elfs/hello");

    assert_eq!(true, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}


#[test]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn successful_run_can_enforce_backtrace() {
    let run_result =
        run_and_truncate("--chip nRF52840_xxAA tests/test_elfs/hello --backtrace=always");

    assert_eq!(true, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}

#[test]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn stack_overflow_is_reported_as_such() {
    let run_result = run_and_truncate("--chip nRF52840_xxAA tests/test_elfs/overflow");

    assert_eq!(false, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}

#[test]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn panic_is_reported_as_such() {
    let run_result = run_and_truncate("--chip nRF52840_xxAA tests/test_elfs/panic");

    assert_eq!(false, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}

#[test]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn panic_verbose() {
    // record current verbose backtrace to catch deviations
    let run_result = run_and_truncate("--chip nRF52840_xxAA tests/test_elfs/panic --verbose");

    assert_eq!(false, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}

#[test]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn unsuccessful_run_can_suppress_backtrace() {
    let run_result =
        run_and_truncate("--chip nRF52840_xxAA tests/test_elfs/panic --backtrace=never");

    assert_eq!(false, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}

#[test]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn stack_overflow_can_suppress_backtrace() {
    let run_result = run_and_truncate("--chip nRF52840_xxAA tests/test_elfs/overflow --backtrace=never");

    assert_eq!(false, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}