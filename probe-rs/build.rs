use std::env;
use std::fs::{read_dir, read_to_string};
use std::io;
use std::path::{Path, PathBuf};

use probe_rs_target::ChipFamily;

fn main() {
    // Test if we have to generate built-in targets
    if env::var("CARGO_FEATURE_BUILTIN_TARGETS").is_err() {
        return;
    }

    let mut families: Vec<ChipFamily> = Vec::new();

    let mut files = vec![];
    visit_dirs(&Path::new("targets"), &mut files).unwrap();
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

    let _: Vec<ChipFamily> = bincode::deserialize(&families_bin).unwrap();
}

/// One possible implementation of walking a directory only visiting files.
fn visit_dirs(dir: &Path, targets: &mut Vec<PathBuf>) -> io::Result<()> {
    if dir.is_dir() {
        for entry in read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit_dirs(&path, targets)?;
            } else {
                targets.push(path.to_owned());
            }
        }
    }
    Ok(())
}
