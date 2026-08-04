use std::ops::Range;

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::RpcResult;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Schema)]
pub struct JEP106Code {
    /// JEP106 identification code.
    /// Points to a manufacturer name in the bank table corresponding to `cc`.
    pub id: u8,
    /// JEP106 continuation code.
    /// This code represents the bank which the manufacturer for a corresponding `id` has to be looked up.
    pub cc: u8,
}

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct ChipFamily {
    /// This is the name of the chip family in base form.
    /// E.g. `nRF52832`.
    pub name: String,
    /// The JEP106 code of the manufacturer.
    pub manufacturer: Option<JEP106Code>,
    /// This vector holds all the variants of the family.
    pub variants: Vec<Chip>,
}

/// A single chip variant.
///
/// This describes an exact chip variant, including the cores, flash and memory size. For example,
/// the `nRF52832` chip has two variants, `nRF52832_xxAA` and `nRF52832_xxBB`. For this case,
/// the struct will correspond to one of the variants, e.g. `nRF52832_xxAA`.
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(deny_unknown_fields)]
pub struct Chip {
    /// This is the name of the chip in base form.
    /// E.g. `nRF52832`.
    pub name: String,
}

pub type ListFamiliesResponse = RpcResult<Vec<ChipFamily>>;

#[derive(Serialize, Deserialize, Schema)]
pub struct ChipInfoRequest {
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct ChipData {
    pub cores: Vec<Core>,
    pub memory_map: Vec<MemoryRegion>,
}

/// An individual core inside a chip
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct Core {
    /// The core name.
    pub name: String,

    /// The core type.
    pub core_type: CoreType,
}

/// Type of a supported core.
#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub enum CoreType {
    /// ARMv6-M: Cortex M0, M0+, M1
    Armv6m,
    /// ARMv7-A: Cortex A7, A9, A15
    Armv7a,
    /// ARMv7-R: Cortex R4, R5, R7, R8
    Armv7r,
    /// ARMv7-M: Cortex M3
    Armv7m,
    /// ARMv7e-M: Cortex M4, M7
    Armv7em,
    /// ARMv8-A: Cortex A35, A55, A72
    Armv8a,
    /// ARMv8-M: Cortex M23, M33
    Armv8m,
    /// RISC-V (32-bit)
    Riscv,
    /// RISC-V (64-bit)
    Riscv64,
    /// Xtensa - TODO: may need to split into NX, LX6 and LX7
    Xtensa,
}

/// Declares the type of a memory region.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Schema)]
pub enum MemoryRegion {
    /// Memory region describing RAM.
    Ram(RamRegion),
    /// Generic memory region, which is neither flash nor RAM.
    Generic(GenericRegion),
    /// Memory region describing flash, EEPROM or other non-volatile memory.
    Nvm(NvmRegion),
}

impl MemoryRegion {
    /// Returns the address range of the memory region.
    pub fn address_range(&self) -> Range<u64> {
        let (start, end) = match self {
            MemoryRegion::Ram(rr) => rr.range,
            MemoryRegion::Generic(gr) => gr.range,
            MemoryRegion::Nvm(nr) => nr.range,
        };
        start..end
    }
}

/// Represents a region in non-volatile memory (e.g. flash or EEPROM).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Schema)]
pub struct NvmRegion {
    /// A name to describe the region
    pub name: Option<String>,
    /// Address range of the region
    pub range: (u64, u64),
    /// List of cores that can access this region
    pub cores: Vec<String>,
    /// True if the memory region is an alias of a different memory region.
    pub is_alias: bool,
    /// Access permissions for the region.
    pub access: Option<MemoryAccess>,
}

/// Represents a region in RAM.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Schema)]
pub struct RamRegion {
    /// A name to describe the region
    pub name: Option<String>,
    /// Address range of the region
    pub range: (u64, u64),
    /// List of cores that can access this region
    pub cores: Vec<String>,
    /// Access permissions for the region.
    #[serde(default)]
    pub access: Option<MemoryAccess>,
}

/// Represents a generic region.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Schema)]
pub struct GenericRegion {
    /// A name to describe the region
    pub name: Option<String>,
    /// Address range of the region
    pub range: (u64, u64),
    /// List of cores that can access this region
    pub cores: Vec<String>,
    /// Access permissions for the region.
    pub access: Option<MemoryAccess>,
}

/// Represents access permissions of a region in RAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Schema)]
pub struct MemoryAccess {
    /// True if the region is readable.
    pub read: bool,
    /// True if the region is writable.
    pub write: bool,
    /// True if the region is executable.
    pub execute: bool,
    /// True if the chip boots from this memory
    pub boot: bool,
}

impl Default for MemoryAccess {
    fn default() -> Self {
        MemoryAccess {
            read: true,
            write: true,
            execute: true,
            boot: false,
        }
    }
}

pub type ChipInfoResponse = RpcResult<ChipData>;

// Used to avoid uploading a temp file to the remote.
#[derive(Serialize, Deserialize, Schema)]
pub struct LoadChipFamilyRequest {
    /// Chip description in YAML format.
    // TODO: instead, serialize the whole ChipFamily struct
    pub families_yaml: String,
}
