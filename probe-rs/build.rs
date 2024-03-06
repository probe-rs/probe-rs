use std::env;
use std::fs::read_dir;
use std::path::Path;

use probe_rs_target::ChipFamily;

fn main() {
    #[cfg(feature = "cli")]
    generate_meta();
    println!("cargo:rerun-if-changed=build.rs");

    // Only rerun build.rs if something inside targets/ or `PROBE_RS_TARGETS_DIR`
    // has changed. (By default cargo reruns build.rs if any file under the crate
    // root has changed) This improves build times and IDE responsivity when not
    // editing targets.
    println!("cargo:rerun-if-changed=targets");
    println!("cargo:rerun-if-env-changed=PROBE_RS_TARGETS_DIR");

    // Test if we have to generate built-in targets
    if env::var("CARGO_FEATURE_BUILTIN_TARGETS").is_err() {
        return;
    }

    let mut families: Vec<ChipFamily> = Vec::new();
    visit_targets(Path::new("targets"), &mut families).unwrap();

    // Check if there are any additional targets to generate for
    match env::var("PROBE_RS_TARGETS_DIR") {
        Ok(additional_target_dir) => {
            println!("cargo:rerun-if-changed={additional_target_dir}");
            visit_targets(Path::new(&additional_target_dir), &mut families).unwrap();
        }
        Err(_err) => {
            // Do nothing as you dont have to add any other targets
        }
    }

    let families_bin =
        bincode::serialize(&families).expect("Failed to serialize families as bincode");

    let out_dir = env::var("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("targets.bincode");
    std::fs::write(dest_path, &families_bin).unwrap();

    let _: Vec<ChipFamily> = match bincode::deserialize(&families_bin) {
        Ok(chip_families) => chip_families,
        Err(deserialize_error) => panic!(
            "Failed to deserialize supported target definitions from bincode: {deserialize_error:?}"
        ),
    };
}

/// One possible implementation of walking a directory only visiting files.
fn visit_targets(dir: &Path, targets: &mut Vec<ChipFamily>) -> anyhow::Result<()> {
    // We make sure the root path we look at is a directory.
    if dir.is_dir() {
        // We enumerate all the entries in the root path.
        for entry in read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            targets.push(ChipFamily::load(&path)?);
        }
    }
    Ok(())
}

/// Generates a `meta.rs` file in a crates `OUT_DIR`.
///
/// This is intended to be used in the `build.rs` of a crate.
///
///
///
/// # Examples
///
/// ```no_run
/// crate::util::meta::generate_meta();
/// println!("cargo:rerun-if-changed=build.rs");
/// ```
#[cfg(feature = "cli")]
pub fn generate_meta() {
    const CARGO_VERSION: &str = env!("CARGO_PKG_VERSION");
    const GIT_VERSION: &str = git_version::git_version!(fallback = "crates.io");
    let long_version: String = format!("{CARGO_VERSION}\ngit commit: {GIT_VERSION}");

    let out_dir = std::env::var_os("OUT_DIR").unwrap();
    let dest_path = std::path::Path::new(&out_dir).join("meta.rs");
    std::fs::write(
        dest_path,
        format!(
            r#"#[allow(dead_code)]mod meta {{
pub const CARGO_VERSION: &str = "{CARGO_VERSION}";
pub const GIT_VERSION: &str = "{GIT_VERSION}";
pub const LONG_VERSION: &str = "{long_version}";
}}        "#
        ),
    )
    .unwrap();
}
