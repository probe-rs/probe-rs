/// CRC32C Binary Metadata Management
/// 
/// This module handles loading and parsing metadata for CRC32C binaries
/// that are automatically generated during the build process.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct CrcBinaryMetadata {
    pub binary: BinaryInfo,
    #[allow(dead_code)] // Preserved for future architecture-specific build info
    pub build_info: BuildInfo,
}

#[derive(Debug, Deserialize)]
pub struct BinaryInfo {
    #[allow(dead_code)] // May be used for multi-architecture support
    pub target: String,
    pub size_bytes: u64,
    pub crc32_function_offset: String,
    #[allow(dead_code)] // Preserved for documentation
    pub algorithm: String,
    #[allow(dead_code)] // Preserved for documentation
    pub library: String,
    #[allow(dead_code)] // Preserved for documentation
    pub entry_point: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // All fields preserved for future architecture-specific builds
pub struct BuildInfo {
    pub generated_by: String,
    pub rust_target: String,
    pub compiler_flags: String,
    pub linker_script: String,
}

impl CrcBinaryMetadata {
    /// Load metadata for a specific target from the embedded TOML content
    pub fn load_for_target(target: &str) -> Result<Self, String> {
        // Always use universal ARM binary metadata for all ARM targets
        let toml_content = match target {
            "thumbv6m-none-eabi" => {
                include_str!("../../../crc32_algorithms/thumbv6m-none-eabi.toml")
            }
            // Legacy support: map old targets to universal ARM binary
            "thumbv7em-none-eabi" | "thumbv7em-none-eabihf" => {
                include_str!("../../../crc32_algorithms/thumbv6m-none-eabi.toml")
            }
            _ => return Err(format!("No metadata available for target: {}", target)),
        };

        toml::from_str(toml_content)
            .map_err(|e| format!("Failed to parse metadata for {}: {}", target, e))
    }

    /// Get the CRC32 function offset as a u32
    pub fn crc32_offset(&self) -> Result<u32, String> {
        let offset_str = &self.binary.crc32_function_offset;
        
        // Handle both "0x12345678" and "12345678" formats
        let offset_str = if offset_str.starts_with("0x") {
            &offset_str[2..]
        } else {
            offset_str
        };
        
        u32::from_str_radix(offset_str, 16)
            .map_err(|e| format!("Failed to parse offset '{}': {}", self.binary.crc32_function_offset, e))
    }

    /// Get the expected binary size
    pub fn binary_size(&self) -> u64 {
        self.binary.size_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_offset_parsing() {
        let metadata = CrcBinaryMetadata {
            binary: BinaryInfo {
                target: "test".to_string(),
                size_bytes: 1234,
                crc32_function_offset: "0x00000008".to_string(),
                algorithm: "CRC32C".to_string(),
                library: "crcxx".to_string(),
                entry_point: "calculate_crc32".to_string(),
            },
            build_info: BuildInfo {
                generated_by: "test".to_string(),
                rust_target: "test".to_string(),
                compiler_flags: "test".to_string(),
                linker_script: "test".to_string(),
            },
        };

        assert_eq!(metadata.crc32_offset().unwrap(), 0x08);
        assert_eq!(metadata.binary_size(), 1234);
    }
}