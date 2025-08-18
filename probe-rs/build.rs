/// Build script for probe-rs
/// Compiles target definitions and validates CRC32 algorithm binaries

use std::path::PathBuf;


/// Binary size validation bounds for CRC32 algorithm binaries
const MIN_CRC_BINARY_SIZE: u64 = 50;
const MAX_CRC_BINARY_SIZE: u64 = 10_000;

/// ARM target specifications for CRC32 validation
/// Now using universal ARM binary (thumbv6m) for all ARM targets
const ARM_TARGETS: &[(&str, &str)] = &[
    ("thumbv6m-none-eabi", "Universal ARM (M0/M0+/M3/M4/M7)"), // Works on all ARM Cortex-M due to upward compatibility
];

/// CRC binary validation logic that can be unit tested
pub struct CrcBinaryValidator {
    min_size: u64,
    max_size: u64,
}

impl CrcBinaryValidator {
    pub fn new(min_size: u64, max_size: u64) -> Self {
        Self { min_size, max_size }
    }

    pub fn validate_binary_size(&self, size: u64) -> Result<(), String> {
        if size < self.min_size {
            Err(format!("Binary too small: {} bytes (minimum: {} bytes)", size, self.min_size))
        } else if size > self.max_size {
            Err(format!("Binary too large: {} bytes (maximum: {} bytes)", size, self.max_size))
        } else {
            Ok(())
        }
    }

    pub fn validate_metadata_consistency(
        metadata_size: u64,
        actual_size: u64,
    ) -> Result<(), String> {
        if metadata_size != actual_size {
            Err(format!(
                "Size mismatch: metadata says {} bytes, actual binary is {} bytes",
                metadata_size, actual_size
            ))
        } else {
            Ok(())
        }
    }
}

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

    // Handle builtin targets (generates targets.bincode)
    handle_builtin_targets();

    // Check for required CRC32 algorithm binaries
    println!("cargo:rerun-if-changed=../crc32_algorithms/");
    check_crc_binaries();
}

fn check_crc_binaries() {
    let mut missing_files = Vec::new();
    let mut invalid_files = Vec::new();
    
    let validator = CrcBinaryValidator::new(MIN_CRC_BINARY_SIZE, MAX_CRC_BINARY_SIZE);
    
    // Check each required binary file
    for (target, description) in ARM_TARGETS {
        let bin_path = format!("../crc32_algorithms/{}.bin", target);
        let toml_path = format!("../crc32_algorithms/{}.toml", target);
        
        // Check binary file
        match std::fs::metadata(&bin_path) {
            Ok(metadata) => {
                let size = metadata.len();
                if let Err(error) = validator.validate_binary_size(size) {
                    invalid_files.push(format!("{} - {}", bin_path, error));
                }
            }
            Err(_) => {
                missing_files.push(format!("{} ({})", bin_path, description));
            }
        }
        
        // Check metadata file
        if !PathBuf::from(&toml_path).exists() {
            missing_files.push(format!("{} (metadata for {})", toml_path, description));
        }
    }
    
    // Report any issues
    if !missing_files.is_empty() || !invalid_files.is_empty() {
        eprintln!("\n❌ ERROR: CRC32C algorithm binaries are missing or invalid!");
        eprintln!("   These files are required for incremental flash verification.\n");
        
        if !missing_files.is_empty() {
            eprintln!("📂 Missing files:");
            for file in &missing_files {
                eprintln!("   - {}", file);
            }
            eprintln!();
        }
        
        if !invalid_files.is_empty() {
            eprintln!("🔧 Invalid files (wrong size):");
            for file in &invalid_files {
                eprintln!("   - {}", file);
            }
            eprintln!();
        }
        
        eprintln!("🛠️  How to build the missing binaries:");
        eprintln!("   cd crc32_algorithms");
        eprintln!("   ./build.sh");
        eprintln!();
        eprintln!("💡 This will build all required CRC32C binaries for ARM targets.");
        eprintln!("   See crc32_algorithms/README.md for more details.\n");
        
        panic!("Build failed: CRC32C binaries are required but missing/invalid");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc_binary_size_validation() {
        let validator = CrcBinaryValidator::new(50, 10_000);
        
        // Too small
        assert!(validator.validate_binary_size(49).is_err());
        let error = validator.validate_binary_size(49).unwrap_err();
        assert!(error.contains("too small"));
        assert!(error.contains("49 bytes"));
        assert!(error.contains("minimum: 50 bytes"));
        
        // Valid range
        assert!(validator.validate_binary_size(50).is_ok());
        assert!(validator.validate_binary_size(500).is_ok());
        assert!(validator.validate_binary_size(1144).is_ok()); // Optimized firmware size
        assert!(validator.validate_binary_size(188).is_ok());  // Simple word firmware size  
        assert!(validator.validate_binary_size(10_000).is_ok());
        
        // Too large
        assert!(validator.validate_binary_size(10_001).is_err());
        let error = validator.validate_binary_size(10_001).unwrap_err();
        assert!(error.contains("too large"));
        assert!(error.contains("10001 bytes"));
        assert!(error.contains("maximum: 10000 bytes"));
        
        // Edge case: 66KB binary (known bad case from objcopy issues)
        assert!(validator.validate_binary_size(66_000).is_err());
    }

    #[test]
    fn test_metadata_consistency_validation() {
        // Matching sizes should pass
        assert!(CrcBinaryValidator::validate_metadata_consistency(432, 432).is_ok());
        assert!(CrcBinaryValidator::validate_metadata_consistency(188, 188).is_ok());
        assert!(CrcBinaryValidator::validate_metadata_consistency(1144, 1144).is_ok());
        
        // Mismatched sizes should fail
        let result = CrcBinaryValidator::validate_metadata_consistency(432, 66_000);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.contains("Size mismatch"));
        assert!(error.contains("metadata says 432 bytes"));
        assert!(error.contains("actual binary is 66000 bytes"));
        
        // Zero sizes
        assert!(CrcBinaryValidator::validate_metadata_consistency(0, 0).is_ok());
        assert!(CrcBinaryValidator::validate_metadata_consistency(0, 100).is_err());
        assert!(CrcBinaryValidator::validate_metadata_consistency(100, 0).is_err());
    }

    #[test]
    fn test_known_firmware_sizes() {
        let validator = CrcBinaryValidator::new(MIN_CRC_BINARY_SIZE, MAX_CRC_BINARY_SIZE);
        
        // Current known good firmware sizes should validate
        assert!(validator.validate_binary_size(1144).is_ok(), "Optimized firmware size should be valid");
        assert!(validator.validate_binary_size(96).is_ok(), "Simple firmware size should be valid");  
        assert!(validator.validate_binary_size(188).is_ok(), "Simple word firmware size should be valid");
        
        // Common problematic sizes should fail
        assert!(validator.validate_binary_size(66_000).is_err(), "Large objcopy artifact should fail");
        assert!(validator.validate_binary_size(10).is_err(), "Tiny binary should fail");
    }

    #[test]
    fn test_validator_constructor() {
        // Normal constructor
        let validator = CrcBinaryValidator::new(100, 5000);
        assert!(validator.validate_binary_size(100).is_ok());
        assert!(validator.validate_binary_size(5000).is_ok());
        assert!(validator.validate_binary_size(99).is_err());
        assert!(validator.validate_binary_size(5001).is_err());
        
        // Edge case: min == max
        let validator = CrcBinaryValidator::new(1000, 1000);
        assert!(validator.validate_binary_size(1000).is_ok());
        assert!(validator.validate_binary_size(999).is_err());
        assert!(validator.validate_binary_size(1001).is_err());
    }
}

