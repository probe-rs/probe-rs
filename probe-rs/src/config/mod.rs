mod chip;
mod chip_family;
mod chip_info;
mod flash_algorithm;
mod flash_properties;
mod memory;
pub mod registry;
mod target;

pub use chip::Chip;
pub use chip_family::ChipFamily;
pub use chip_info::ChipInfo;
pub use flash_algorithm::{FlashAlgorithm, RawFlashAlgorithm};
pub use flash_properties::FlashProperties;
pub use memory::{
    FlashRegion, MemoryRange, MemoryRegion, PageInfo, RamRegion, SectorDescription, SectorInfo,
};
pub use registry::RegistryError;
pub use target::{Target, TargetParseError, TargetSelector};
