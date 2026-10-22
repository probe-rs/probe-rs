//! This build script compiles the target definitions into a binary format.

use std::path::{Path, PathBuf};
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=targets");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("targets.bincode");

    probe_rs_target::process_targets(&[PathBuf::from("targets")], &dest_path);
}
