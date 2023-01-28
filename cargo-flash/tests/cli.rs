use assert_cmd::prelude::*; // Add methods on commands
use predicates::prelude::*; // Used for writing assertions
use std::process::Command; // Run programs

#[test]
fn query_long_version() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("cargo-flash")?;

    cmd.arg("--version");
    cmd.assert()
        .success()
        .stdout(predicate::str::is_match("^cargo-flash \\S+\ngit commit: \\S+\\n$").unwrap());

    Ok(())
}

#[test]
fn query_short_version() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("cargo-flash")?;

    cmd.arg("-V");
    cmd.assert()
        .success()
        .stdout(predicate::str::is_match("^cargo-flash \\S+\\n$").unwrap());

    Ok(())
}
