use std::env;
use std::fs::read_dir;
use std::path::Path;

use probe_rs_target::ChipFamily;

fn main() {
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
