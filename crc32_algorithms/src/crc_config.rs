/// Shared CRC32 algorithm configuration for probe-rs
///
/// This file defines the CRC algorithm used by both the embedded firmware
/// and the host-side verification. Keep this synchronized to ensure
/// compatibility between target and host CRC calculations.
pub use crcxx::crc32::catalog::CRC_32_BZIP2 as CRC_ALGORITHM;

/// Human-readable name for the CRC algorithm (used in metadata)
pub const CRC_ALGORITHM_NAME: &str = "CRC32_BZIP2/Standard";

/// CRC polynomial value for reference
pub const CRC_POLYNOMIAL: u32 = 0x04C11DB7;

/// Whether the algorithm uses input reflection
pub const CRC_REFIN: bool = false;

/// Whether the algorithm uses output reflection  
pub const CRC_REFOUT: bool = false;

/// Initial CRC value
pub const CRC_INIT: u32 = 0xFFFFFFFF;

/// Final XOR value
pub const CRC_XOROUT: u32 = 0xFFFFFFFF;
