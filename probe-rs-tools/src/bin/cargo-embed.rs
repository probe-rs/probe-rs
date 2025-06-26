#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Command, exit};

fn main() {
    let mut args: Vec<_> = std::env::args_os().collect();
    // No matter what the binary is called, we need to pass `cargo-embed` to `probe-rs`.
    args[0] = "cargo-embed".into();
    // Also, skip the very first argument if it's `embed`, because it was inserted by cargo.
    if args.get(1).filter(|s| *s == "embed").is_some() {
        args.remove(1);
    }
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

    eprintln!("Error launching `probe-rs`: {err}");
    eprintln!("Note: the `cargo-embed` binary is a small shim that launches `probe-rs`.");
    eprintln!("Make sure `probe-rs` is installed and available in $PATH.");

    exit(99);
}
