use std::{env, ffi::OsString, path::PathBuf};

const NORDIC_SAMPLE_PACK: &str =
    "tests/test_data/NordicSemiconductor.nRF_DeviceFamilyPack.8.32.1.pack";

struct Command {
    bin: PathBuf,
    args: Vec<OsString>,
}

// Adapted from
// https://github.com/rust-lang/cargo/blob/485670b3983b52289a2f353d589c57fae2f60f82/tests/testsuite/support/mod.rs#L507
fn target_dir() -> PathBuf {
    env::current_exe()
        .ok()
        .map(|mut path| {
            path.pop();
            if path.ends_with("deps") {
                path.pop();
            }
            path
        })
        .unwrap()
}

impl Command {
    fn cargo_bin(name: &str) -> Command {
        let bin = env::var_os(format!("CARGO_BIN_EXE_{name}"))
            .map(|p| p.into())
            .unwrap_or_else(|| target_dir().join(format!("{name}{}", env::consts::EXE_SUFFIX)));

        Command {
            bin,
            args: Vec::new(),
        }
    }

    fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    fn run(self) -> CommandResult {
        let output = std::process::Command::new(self.bin)
            .args(&self.args)
            .output()
            .expect("failed to execute command");

        CommandResult {
            status: output.status,
            stdout: String::from_utf8(output.stdout).expect("stdout is not valid UTF-8"),
            stderr: String::from_utf8(output.stderr).expect("stderr is not valid UTF-8"),
        }
    }
}

struct CommandResult {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

#[test]
fn missing_output_directory() {
    // extract an example pack
    let result = Command::cargo_bin("target-gen")
        .arg("pack")
        .arg(NORDIC_SAMPLE_PACK)
        .run();

    assert!(!result.status.success());
    assert!(result
        .stderr
        .contains("the following required arguments were not provided:"));
}

#[test]
fn extract_target_specs() {
    // create a temporary directory
    let temp = tempfile::TempDir::new().unwrap();

    // extract an example pack
    let result = Command::cargo_bin("target-gen")
        .arg("pack")
        .arg(NORDIC_SAMPLE_PACK)
        .arg(temp.path())
        .run();

    assert!(result.status.success());
    assert!(result.stdout.contains("Generated 4 target definition(s):"));
}
