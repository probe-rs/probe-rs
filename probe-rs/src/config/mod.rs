mod chip;
mod chip_family;
mod chip_info;
mod flash_algorithm;
mod flash_properties;
mod memory;
pub mod registry;
mod target;

pub use chip_family::ChipFamily;
pub use chip_info::ChipInfo;
pub use chip::Chip;
pub use registry::RegistryError;
pub use target::{Target, TargetSelector, TargetParseError};
pub use memory::{MemoryRegion, FlashRegion, RamRegion, MemoryRange, SectorDescription, PageInfo, SectorInfo};
pub use flash_algorithm::{RawFlashAlgorithm, FlashAlgorithm};
pub use flash_properties::FlashProperties;