//! Target description schema
//!
//! For debugging and flashing different chips, called *target* in probe-rs, some
//! target specific configuration is required. This includes the architecture of
//! the chip, e.g. RISC-V or ARM, and information about the memory map of the target,
//! which can be used together with a flash algorithm to program the flash memory
//! of a target.
//!
//! This crate contains the schema structs for the YAML target description files.
//!
#![warn(missing_docs)]

mod chip;
pub mod chip_detection;
mod chip_family;
mod flash_algorithm;
mod flash_properties;
mod memory;
pub(crate) mod serialize;

pub use chip::{
    ApAddress, ArmCoreAccessOptions, Chip, Core, CoreAccessOptions, Jtag, RiscvCoreAccessOptions,
    RiscvJtagTunnel, ScanChainElement, XtensaCoreAccessOptions,
};
pub use chip_family::{
    Architecture, ChipFamily, CoreType, Endian, InstructionSet, TargetDescriptionSource,
};
pub use flash_algorithm::{RawFlashAlgorithm, TransferEncoding};
pub use flash_properties::FlashProperties;
pub use memory::{
    GenericRegion, MemoryAccess, MemoryRange, MemoryRegion, NvmRegion, PageInfo, RamRegion,
    RegionMergeIterator, SectorDescription, SectorInfo,
};

#[cfg(feature = "bincode")]
mod builtin_targets {
    use std::fs::{read_dir, read_to_string};
    use std::io;
    use std::path::{Path, PathBuf};

    use crate::ChipFamily;

    /// Process target yamls at the given source paths, and produce a bincode-encoded file at the destination path.
    pub fn process_targets(source_paths: &[PathBuf], dest_path: &Path) {
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

        for path in source_paths {
            visit_dirs(path, &mut process_target_yaml).unwrap();
        }

        let config = bincode::config::standard();
        let families_bin = bincode::serde::encode_to_vec(&families, config)
            .expect("Failed to serialize families as bincode");

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

#[cfg(feature = "bincode")]
pub use builtin_targets::process_targets;
