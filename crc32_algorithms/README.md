# CRC32 Universal ARM Binary for probe-rs

This directory contains a self-contained build system for generating the universal CRC32 algorithm binary used by probe-rs for incremental flash verification.

## Contents

- **Binary file** (`thumbv6m-none-eabi.bin`): Universal ARM CRC32 binary (works on all Cortex-M cores)
- **Metadata file** (`thumbv6m-none-eabi.toml`): Configuration and metadata for the binary
- **Source code** (`src/bin/firmware_crcxx.rs`): The CRC32_BZIP2 implementation using crcxx crate
- **Build system**: Complete standalone Cargo project for building the binary

## Building and Cleaning

### Clean Build Artifacts

To clean all generated binaries and build artifacts:

```bash
# From probe-rs root directory
cargo xtask clean-crc32
```

This will:
1. Remove target/ directory with all Rust build artifacts  
2. Remove generated binary files (*.bin, *.toml)
3. Leave source code unchanged

### Build the Binary

To rebuild the binary (e.g., after modifying the source):

```bash  
# From probe-rs root directory (recommended)
cargo xtask build-crc32 --arm-only

# OR manually from crc32_algorithms directory
cd crc32_algorithms
RUSTFLAGS="-C link-arg=-Tlink_minimal.x -C panic=abort" cargo build --release --bin crc32_firmware_crcxx --target thumbv6m-none-eabi
arm-none-eabi-objcopy -O binary target/thumbv6m-none-eabi/release/crc32_firmware_crcxx thumbv6m-none-eabi.bin
```

The xtask approach will:
1. Build the universal ARM binary (thumbv6m-none-eabi)
2. Extract the raw binary using arm-none-eabi-objcopy
3. Generate metadata TOML file with size, checksum, and build info
4. Place outputs in this directory

## Universal ARM Binary

probe-rs uses a single universal ARM binary that works on all Cortex-M cores:

| Binary | Target Architecture | Actual Size | Entry Point | Compatible With |
|--------|---------------------|-------------|-------------|-----------------|
| thumbv6m-none-eabi.bin | ARMv6-M | 1144 bytes | 0x08 | All Cortex-M (M0/M0+/M3/M4/M7) |

This approach leverages ARM's upward compatibility - ARMv6-M instructions are fully supported on ARMv7-M and ARMv7E-M cores.

## Prerequisites

- Rust toolchain with ARM target:
  ```bash
  rustup target add thumbv6m-none-eabi
  ```
- ARM toolchain for objcopy:
  - Ubuntu/Debian: `sudo apt install gcc-arm-none-eabi`
  - macOS: `brew install --cask gcc-arm-embedded`

## Implementation Details

The binaries implement CRC32_BZIP2 (Standard) with polynomial 0x04C11DB7 using:
- crcxx crate's optimized 256-entry lookup table (no reflections for better performance)
- Position-independent code (PIC) for loading at any RAM address
- Minimal size optimization (opt-level = "s", LTO enabled)
- No debug information for smallest possible size

### Performance Improvement

CRC32_BZIP2 was chosen over CRC32C (ISCSI) for better performance:
- **6.7% faster** on ARM Cortex-M0+ (RP2040): 108.2ms vs 115.9ms per 4KB sector
- **Eliminates bit reflections** on input and output for reduced CPU overhead
- **Same binary size** (1144 bytes) - dominated by 256-entry lookup table
- **Standard polynomial** enables potential hardware acceleration

## Integration with probe-rs

These binaries are automatically included in probe-rs builds via:
```rust
const M0_CRC32_BLOB: &[u8] = include_bytes!("../../../crc32_algorithms/thumbv6m-none-eabi.bin");
```

The main probe-rs build process expects these files to exist in this directory.