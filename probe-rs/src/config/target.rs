use super::chip::Chip;
use super::flash_algorithm::{FlashAlgorithm, RawFlashAlgorithm};
use super::memory::{FlashRegion, MemoryRegion, RamRegion};
use super::registry::TargetIdentifier;
use crate::target::Core;

/// This describes a complete target with a fixed chip model and variant.
#[derive(Debug, Clone)]
pub struct Target {
    /// The complete identifier of the target.
    pub identifier: TargetIdentifier,
    /// The name of the flash algorithm.
    pub flash_algorithm: Option<FlashAlgorithm>,
    /// The core type.
    pub core: Box<dyn Core>,
    /// The memory map of the target.
    pub memory_map: Vec<MemoryRegion>,
}

pub type TargetParseError = serde_yaml::Error;

impl Target {
    pub fn new(
        chip: &Chip,
        ram: &RamRegion,
        flash: &FlashRegion,
        flash_algorithm: &RawFlashAlgorithm,
        core: Box<dyn Core>,
    ) -> Target {
        Target {
            identifier: TargetIdentifier {
                chip_name: chip.name.clone(),
                flash_algorithm_name: Some(flash_algorithm.name.clone()),
            },
            flash_algorithm: Some(flash_algorithm.assemble(ram, flash)),
            core,
            memory_map: chip.memory_map.clone(),
        }
    }
}
