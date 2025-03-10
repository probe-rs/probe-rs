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
    builtin_targets::process();
}

#[cfg(feature = "builtin-targets")]
mod builtin_targets {

    use std::env;
    use std::fs::{read_dir, read_to_string};
    use std::io;
    use std::path::Path;

    use probe_rs_target::ChipFamily;

    pub fn process() {
        let mut families = Vec::new();
        let mut process_target_yaml = |file: &Path| {
            let string = read_to_string(file).unwrap_or_else(|error| {
                panic!(
                    "Failed to read target file {} because:\n{error}",
                    file.display()
                )
            });

            match serde_yaml::from_str::<ChipFamily>(&string) {
                Ok(family) => families.push(family),
                Err(error) => panic!(
                    "Failed to parse target file: {} because:\n{error}",
                    file.display()
                ),
            }
        };

        visit_dirs("targets", &mut process_target_yaml).unwrap();

        // Check if there are any additional targets to generate for
        if let Ok(additional_target_dir) = env::var("PROBE_RS_TARGETS_DIR") {
            println!("cargo:rerun-if-changed={additional_target_dir}");
            visit_dirs(additional_target_dir, &mut process_target_yaml).unwrap();
        }

        let config = bincode::config::standard();
        let families_bin = bincode::serde::encode_to_vec(&families, config)
            .expect("Failed to serialize families as bincode");

        let out_dir = env::var("OUT_DIR").unwrap();
        let dest_path = Path::new(&out_dir).join("targets.bincode");
        std::fs::write(dest_path, &families_bin).unwrap();

        // Check if we can deserialize the bincode again, otherwise the binary will not be usable.
        if let Err(deserialize_error) =
            bincode::serde::decode_from_slice::<Vec<ChipFamily>, _>(&families_bin, config)
        {
            panic!(
                "Failed to deserialize supported target definitions from bincode: {deserialize_error:?}"
            );
        }
    }

    /// Call `process` on all files in a directory and its subdirectories.
    fn visit_dirs(dir: impl AsRef<Path>, process: &mut impl FnMut(&Path)) -> io::Result<()> {
        // Inner function to avoid generating multiple implementations for the different path types.
        fn visit_dirs_impl(dir: &Path, process: &mut impl FnMut(&Path)) -> io::Result<()> {
            for entry in read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    visit_dirs_impl(&path, process)?;
                } else {
                    process(&path);
                }
            }

            Ok(())
        }

        let dir = dir.as_ref();
        if !dir.is_dir() {
            return Ok(());
        }

        visit_dirs_impl(dir, process)
    }
}
