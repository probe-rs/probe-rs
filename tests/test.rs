use os_pipe::pipe;
use serial_test::serial;
use std::{
    io::Read,
    process::{Command, ExitStatus},
};

struct RunResult {
    exit_status: ExitStatus,
    output: String,
}

/// run probe-run with `args` and truncate the "Finished .. in .." and "Running `...`" flashing output
/// NOTE: this currently only capures `stdin`, so any `log::` ed output, like flashing
fn run(args: &str) -> RunResult {
    let (mut reader, mut handle) = run_command(args);

    // retrieve output and clean up
    let mut probe_run_output = String::new();
    reader.read_to_string(&mut probe_run_output).unwrap();
    let exit_status = handle.wait().unwrap();

    // remove the lines printed during flashing, as they contain timing info that's not always the same
    let output = truncate_output(probe_run_output);

    RunResult {
        exit_status,
        output,
    }
}

#[cfg(target_family = "unix")]
// runs command with `args` and terminates after `timeout_s` seconds.
fn run_and_terminate(args: &str, timeout_s: u64) -> RunResult {
    let (mut reader, mut handle) = run_command(args);

    // sleep a bit so that child can process the input
    std::thread::sleep(std::time::Duration::from_secs(timeout_s));

    // send SIGINT to the child
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(handle.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .expect("cannot send ctrl-c");

    // retrieve output and clean up
    let mut probe_run_output = String::new();
    reader.read_to_string(&mut probe_run_output).unwrap();
    let exit_status = handle.wait().unwrap();

    let output = truncate_output(probe_run_output);

    RunResult {
        exit_status,
        output,
    }
}

fn run_command(args: &str) -> (os_pipe::PipeReader, std::process::Child) {
    // add prefix to run this repository's version of `probe-run` and
    // remove user-dependent registry and rustc information from backtrace paths
    let complete_command = format!("run -- {} --shorten-paths", args);

    let (reader, writer) = pipe().unwrap();
    let writer_clone = writer.try_clone().unwrap();

    let handle = Command::new("cargo")
        .args(complete_command.split(" "))
        // capture stderr and stdout while preserving line order
        .stdout(writer)
        .stderr(writer_clone)
        // run `probe-run`
        .spawn()
        .unwrap();
    (reader, handle)
}

// remove the lines printed during flashing, as they contain timing info that's not always the same
fn truncate_output(probe_run_output: String) -> String {
    probe_run_output
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
        .collect()
}

#[test]
#[serial]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn successful_run_has_no_backtrace() {
    let run_result = run("--chip nRF52840_xxAA tests/test_elfs/hello-rzcobs");

    assert_eq!(true, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}

#[test]
#[serial]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn raw_encoding() {
    let run_result = run("--chip nRF52840_xxAA tests/test_elfs/hello-raw");

    assert_eq!(true, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}

#[test]
#[serial]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn successful_run_can_enforce_backtrace() {
    let run_result = run("--chip nRF52840_xxAA tests/test_elfs/hello-rzcobs --backtrace=always");

    assert_eq!(true, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}

#[test]
#[serial]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn stack_overflow_is_reported_as_such() {
    let run_result = run("--chip nRF52840_xxAA tests/test_elfs/overflow-rzcobs");

    assert_eq!(false, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}

#[test]
#[serial]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn panic_is_reported_as_such() {
    let run_result = run("--chip nRF52840_xxAA tests/test_elfs/panic-rzcobs");

    assert_eq!(false, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}

#[test]
#[serial]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn panic_verbose() {
    // record current verbose backtrace to catch deviations
    let run_result = run("--chip nRF52840_xxAA tests/test_elfs/panic-rzcobs --verbose");

    assert_eq!(false, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}

#[test]
#[serial]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn unsuccessful_run_can_suppress_backtrace() {
    let run_result = run("--chip nRF52840_xxAA tests/test_elfs/panic-rzcobs --backtrace=never");

    assert_eq!(false, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}

#[test]
#[serial]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
fn stack_overflow_can_suppress_backtrace() {
    let run_result = run("--chip nRF52840_xxAA tests/test_elfs/overflow-rzcobs --backtrace=never");

    assert_eq!(false, run_result.exit_status.success());
}

#[test]
#[serial]
// this test should not be run by default, as it requires the target hardware to be present
#[ignore]
#[cfg(target_family = "unix")]
fn ctrl_c_by_user_is_reported_as_such() {
    let run_result =
        run_and_terminate("--chip nRF52840_xxAA tests/test_elfs/silent-loop-rzcobs", 5);

    assert_eq!(false, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}
