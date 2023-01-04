use std::env;
use std::fs::{read_dir, read_to_string};
use std::io;
use std::path::{Path, PathBuf};

use probe_rs_target::ChipFamily;

fn main() {
    // Only rerun build.rs if something inside targets/ has changed. (By default
    // cargo reruns build.rs if any file under the crate root has changed)
    // This improves build times and IDE responsivity when not editing targets.
    println!("cargo:rerun-if-changed=targets");

    // Test if we have to generate built-in targets
    if env::var("CARGO_FEATURE_BUILTIN_TARGETS").is_err() {
        return;
    }

    let mut families: Vec<ChipFamily> = Vec::new();

    let mut files = vec![];
    visit_dirs(Path::new("targets"), &mut files).unwrap();

    // Check if there are any additional targets to generate for
    match env::var("PROBE_RS_TARGETS_DIR") {
        Ok(additional_target_dir) => {
            visit_dirs(Path::new(&additional_target_dir), &mut files).unwrap();
        }
        Err(_err) => {
            // Do nothing as you dont have to add any other targets
        }
    }

    for file in files {
        let string = read_to_string(&file).expect(
            "Algorithm definition file could not be read. This is a bug. Please report it.",
        );

        let yaml: Result<ChipFamily, _> = serde_yaml::from_str(&string);

        match yaml {
            Ok(familiy) => families.push(familiy),
            Err(e) => panic!("Failed to parse target file: {:?} because:\n{}", file, e),
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
            "Failed to deserialize supported target definitions from bincode: {:?}",
            deserialize_error
        ),
    };
}

/// One possible implementation of walking a directory only visiting files.
fn visit_dirs(dir: &Path, targets: &mut Vec<PathBuf>) -> io::Result<()> {
    if dir.is_dir() {
        for entry in read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit_dirs(&path, targets)?;
            } else if let Some(extension) = path.extension() {
                if extension.eq_ignore_ascii_case("yaml") {
                    targets.push(path);
                }
            }
        }
    }
    Ok(())
}
