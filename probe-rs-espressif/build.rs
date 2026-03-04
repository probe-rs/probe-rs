//! This build script compiles the target definitions into a binary format.

use std::path::{Path, PathBuf};
fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Only rerun build.rs if something inside targets/ has changed. (By default
    // cargo reruns build.rs if any file under the crate root has changed) This
    // improves build times and IDE responsivity when not editing targets.
    println!("cargo:rerun-if-changed=targets");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("targets.bincode");

    probe_rs_target::process_targets(&[PathBuf::from("targets")], &dest_path);
}
