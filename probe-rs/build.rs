//! This build script compiles the target definitions into a binary format.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // The `probers_docsrs` config is used to build docs for docs.rs.
    // We can't use just `docsrs` because using that leads to a compile
    // error in hidapi, see <https://github.com/ruabmbua/hidapi-rs/pull/158>.
    println!("cargo::rustc-check-cfg=cfg(probers_docsrs)");

    // Only rerun build.rs if something inside targets/ or `PROBE_RS_TARGETS_DIR`
    // has changed. (By default cargo reruns build.rs if any file under the crate
    // root has changed) This improves build times and IDE responsivity when not
    // editing targets.
    println!("cargo:rerun-if-changed=targets");
    println!("cargo:rerun-if-env-changed=PROBE_RS_TARGETS_DIR");

    handle_builtin_targets();
}

#[cfg(not(feature = "builtin-targets"))]
fn handle_builtin_targets() {
    // Nothing to do here
}

#[cfg(feature = "builtin-targets")]
fn handle_builtin_targets() {
    use std::{
        env,
        path::{Path, PathBuf},
    };

    let out_dir = env::var("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("targets.bincode");

    let mut source_paths = vec![PathBuf::from("targets")];

    // Check if there are any additional targets to generate for
    if let Ok(additional_target_dir) = env::var("PROBE_RS_TARGETS_DIR") {
        println!("cargo:rerun-if-changed={additional_target_dir}");
        source_paths.push(PathBuf::from(&additional_target_dir));
    }

    probe_rs_target::process_targets(&source_paths, &dest_path);
}
