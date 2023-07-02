#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{exit, Command};

fn main() {
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    args.insert(0, "cargo-embed".into());
    let mut cmd = Command::new("probe-rs");
    cmd.args(&args);

    #[cfg(unix)]
    let err = cmd.exec();
    #[cfg(not(unix))]
    let err = match cmd.spawn() {
        Ok(mut child) => match child.wait() {
            Ok(exitcode) => exit(exitcode.code().unwrap_or(98)),
            Err(e) => e,
        },
        Err(e) => e,
    };

    eprintln!("Error launching `probe-rs`: {}", err);
    eprintln!("Note: the `cargo-embed` binary is a small shim that launches `probe-rs`.");
    eprintln!("Make sure `probe-rs` is installed and available in $PATH.");

    exit(99);
}
