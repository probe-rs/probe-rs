use std::{
    io::Read,
    process::{Command, ExitStatus},
};

use os_pipe::pipe;
use rstest::rstest;
use serial_test::serial;

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
    let cmd = format!("run -- --chip nRF52840_xxAA tests/test_elfs/{args} --shorten-paths");

    let (reader, writer) = pipe().unwrap();
    let writer_clone = writer.try_clone().unwrap();

    let handle = Command::new("cargo")
        .args(cmd.split(' '))
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

#[rstest]
#[case::successful_run_has_no_backtrace("hello-rzcobs", true)]
#[case::raw_encoding("hello-raw", true)]
#[case::successful_run_can_enforce_backtrace("hello-rzcobs --backtrace=always", true)]
#[case::stack_overflow_is_reported_as_such("overflow-rzcobs", false)]
#[case::panic_is_reported_as_such("panic-rzcobs", false)]
#[should_panic] // FIXME: see https://github.com/knurling-rs/probe-run/issues/336
#[case::panic_verbose("panic-rzcobs --verbose", false)]
#[case::unsuccessful_run_can_suppress_backtrace("panic-rzcobs --backtrace=never", false)]
#[case::stack_overflow_can_suppress_backtrace("overflow-rzcobs --backtrace=never", false)]
#[serial]
#[ignore = "requires the target hardware to be present"]
fn snapshot_test(#[case] args: &str, #[case] success: bool) {
    let run_result = run(args);
    assert_eq!(success, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}

#[test]
#[serial]
#[ignore = "requires the target hardware to be present"]
#[cfg(target_family = "unix")]
fn ctrl_c_by_user_is_reported_as_such() {
    let run_result = run_and_terminate("silent-loop-rzcobs", 5);
    assert_eq!(false, run_result.exit_status.success());
    insta::assert_snapshot!(run_result.output);
}
