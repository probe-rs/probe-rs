use std::ops::Range;

use anyhow::Context;
use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::Target;
use serde::{Deserialize, Serialize};

use crate::rpc::functions::{NoResponse, RpcContext, RpcError, RpcResult};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Schema)]
pub struct JEP106Code {
    /// JEP106 identification code.
    /// Points to a manufacturer name in the bank table corresponding to `cc`.
    pub id: u8,
    /// JEP106 continuation code.
    /// This code represents the bank which the manufacturer for a corresponding `id` has to be looked up.
    pub cc: u8,
}

impl From<jep106::JEP106Code> for JEP106Code {
    fn from(value: jep106::JEP106Code) -> Self {
        Self {
            id: value.id,
            cc: value.cc,
        }
    }
}

impl From<JEP106Code> for jep106::JEP106Code {
    fn from(value: JEP106Code) -> Self {
        Self {
            id: value.id,
            cc: value.cc,
        }
    }
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

impl From<probe_rs_target::ChipFamily> for ChipFamily {
    fn from(value: probe_rs_target::ChipFamily) -> Self {
        Self {
            name: value.name,
            manufacturer: value.manufacturer.map(|m| m.into()),
            variants: value.variants.into_iter().map(|v| v.into()).collect(),
        }
    }
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

impl From<probe_rs_target::Chip> for Chip {
    fn from(value: probe_rs_target::Chip) -> Self {
        Self { name: value.name }
    }
}

pub type ListFamiliesResponse = RpcResult<Vec<ChipFamily>>;

pub fn list_families(_ctx: &mut RpcContext, _header: VarHeader, _req: ()) -> ListFamiliesResponse {
    Ok(probe_rs::config::families()
        .into_iter()
        .map(|f| f.into())
        .collect())
}

#[derive(Serialize, Deserialize, Schema)]
pub struct ChipInfoRequest {
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct ChipData {
    pub cores: Vec<Core>,
    pub memory_map: Vec<MemoryRegion>,
}

impl From<Target> for ChipData {
    fn from(value: Target) -> Self {
        Self {
            cores: value.cores.into_iter().map(|core| core.into()).collect(),
            memory_map: value
                .memory_map
                .into_iter()
                .map(|mmap| mmap.into())
                .collect(),
        }
    }
}

/// An individual core inside a chip
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct Core {
    /// The core name.
    pub name: String,

    /// The core type.
    pub core_type: CoreType,
}

impl From<probe_rs_target::Core> for Core {
    fn from(value: probe_rs_target::Core) -> Self {
        Self {
            name: value.name,
            core_type: value.core_type.into(),
        }
    }
}

/// Type of a supported core.
#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub enum CoreType {
    /// ARMv6-M: Cortex M0, M0+, M1
    Armv6m,
    /// ARMv7-A: Cortex A7, A9, A15
    Armv7a,
    /// ARMv7-M: Cortex M3
    Armv7m,
    /// ARMv7e-M: Cortex M4, M7
    Armv7em,
    /// ARMv7-A: Cortex A35, A55, A72
    Armv8a,
    /// ARMv8-M: Cortex M23, M33
    Armv8m,
    /// RISC-V
    Riscv,
    /// Xtensa - TODO: may need to split into NX, LX6 and LX7
    Xtensa,
}

impl From<probe_rs_target::CoreType> for CoreType {
    fn from(value: probe_rs_target::CoreType) -> Self {
        match value {
            probe_rs_target::CoreType::Armv6m => CoreType::Armv6m,
            probe_rs_target::CoreType::Armv7a => CoreType::Armv7a,
            probe_rs_target::CoreType::Armv7m => CoreType::Armv7m,
            probe_rs_target::CoreType::Armv7em => CoreType::Armv7em,
            probe_rs_target::CoreType::Armv8a => CoreType::Armv8a,
            probe_rs_target::CoreType::Armv8m => CoreType::Armv8m,
            probe_rs_target::CoreType::Riscv => CoreType::Riscv,
            probe_rs_target::CoreType::Xtensa => CoreType::Xtensa,
        }
    }
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

impl From<probe_rs_target::MemoryRegion> for MemoryRegion {
    fn from(value: probe_rs_target::MemoryRegion) -> Self {
        match value {
            probe_rs_target::MemoryRegion::Ram(rr) => MemoryRegion::Ram(rr.into()),
            probe_rs_target::MemoryRegion::Generic(gr) => MemoryRegion::Generic(gr.into()),
            probe_rs_target::MemoryRegion::Nvm(nr) => MemoryRegion::Nvm(nr.into()),
        }
    }
}

impl MemoryRegion {
    /// Returns the address range of the memory region.
    pub fn address_range(&self) -> Range<u64> {
        match self {
            MemoryRegion::Ram(rr) => rr.range.clone(),
            MemoryRegion::Generic(gr) => gr.range.clone(),
            MemoryRegion::Nvm(nr) => nr.range.clone(),
        }
    }
}

/// Represents a region in non-volatile memory (e.g. flash or EEPROM).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Schema)]
pub struct NvmRegion {
    /// A name to describe the region
    pub name: Option<String>,
    /// Address range of the region
    pub range: Range<u64>,
    /// List of cores that can access this region
    pub cores: Vec<String>,
    /// True if the memory region is an alias of a different memory region.
    pub is_alias: bool,
    /// Access permissions for the region.
    pub access: Option<MemoryAccess>,
}

impl From<probe_rs_target::NvmRegion> for NvmRegion {
    fn from(value: probe_rs_target::NvmRegion) -> Self {
        Self {
            name: value.name,
            range: value.range,
            cores: value.cores,
            is_alias: value.is_alias,
            access: value.access.map(|a| a.into()),
        }
    }
}

/// Represents a region in RAM.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Schema)]
pub struct RamRegion {
    /// A name to describe the region
    pub name: Option<String>,
    /// Address range of the region
    pub range: Range<u64>,
    /// List of cores that can access this region
    pub cores: Vec<String>,
    /// Access permissions for the region.
    #[serde(default)]
    pub access: Option<MemoryAccess>,
}

impl From<probe_rs_target::RamRegion> for RamRegion {
    fn from(value: probe_rs_target::RamRegion) -> Self {
        Self {
            name: value.name,
            range: value.range,
            cores: value.cores,
            access: value.access.map(|a| a.into()),
        }
    }
}

/// Represents a generic region.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Schema)]
pub struct GenericRegion {
    /// A name to describe the region
    pub name: Option<String>,
    /// Address range of the region
    pub range: Range<u64>,
    /// List of cores that can access this region
    pub cores: Vec<String>,
    /// Access permissions for the region.
    pub access: Option<MemoryAccess>,
}

impl From<probe_rs_target::GenericRegion> for GenericRegion {
    fn from(value: probe_rs_target::GenericRegion) -> Self {
        Self {
            name: value.name,
            range: value.range,
            cores: value.cores,
            access: value.access.map(|a| a.into()),
        }
    }
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

impl From<probe_rs_target::MemoryAccess> for MemoryAccess {
    fn from(value: probe_rs_target::MemoryAccess) -> Self {
        Self {
            read: value.read,
            write: value.write,
            execute: value.execute,
            boot: value.boot,
        }
    }
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

pub fn chip_info(
    _ctx: &mut RpcContext,
    _header: VarHeader,
    request: ChipInfoRequest,
) -> ChipInfoResponse {
    Ok(probe_rs::config::get_target_by_name(request.name)?.into())
}

// Used to avoid uploading a temp file to the remote.
#[derive(Serialize, Deserialize, Schema)]
pub struct LoadChipFamilyRequest {
    pub family_data: Vec<u8>,
}

pub async fn load_chip_family(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: LoadChipFamilyRequest,
) -> NoResponse {
    if !ctx.is_local() {
        return RpcResult::Err(RpcError::from(
            "Loading chip families is not supported in the remote interface yet.",
        ));
    }

    let family = postcard::from_bytes::<probe_rs_target::ChipFamily>(&request.family_data)
        .context("Failed to deserialize chip family data")?;

    // TODO: this can only be done safely if we have separate registries per connection.
    probe_rs::config::add_target_family(family)?;

    Ok(())
}
