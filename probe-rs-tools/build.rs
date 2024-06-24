use std::{env, process::Command};

fn main() {
    generate_bin_versions();
    println!("cargo:rerun-if-changed=build.rs");
}

fn generate_bin_versions() {
    const CARGO_VERSION: &str = env!("CARGO_PKG_VERSION");
    const GIT_VERSION: &str = git_version::git_version!(fallback = "crates.io");

    let git_rev = {
        if let Ok(output) = Command::new("git").args(["rev-parse", "HEAD"]).output() {
            String::from_utf8(output.stdout).unwrap()
        } else {
            "crates.io".to_string()
        }
    };

    println!("cargo:rustc-env=PROBE_RS_VERSION={CARGO_VERSION}");
    println!("cargo:rustc-env=PROBE_RS_LONG_VERSION={CARGO_VERSION} (git commit: {GIT_VERSION})");
    println!("cargo:rustc-env=GIT_REV={}", git_rev);
}
