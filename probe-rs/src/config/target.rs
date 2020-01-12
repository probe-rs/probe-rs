use super::chip::Chip;
use super::flash_algorithm::RawFlashAlgorithm;
use super::memory::MemoryRegion;
use super::registry::TargetIdentifier;
use crate::target::Core;

/// This describes a complete target with a fixed chip model and variant.
#[derive(Debug, Clone)]
pub struct Target {
    /// The complete identifier of the target.
    pub identifier: TargetIdentifier,
    /// The name of the flash algorithm.
    pub flash_algorithms: Vec<RawFlashAlgorithm>,
    /// The core type.
    pub core: Box<dyn Core>,
    /// The memory map of the target.
    pub memory_map: Vec<MemoryRegion>,
}

pub type TargetParseError = serde_yaml::Error;

impl Target {
    pub fn new(
        chip: &Chip,
        flash_algorithms: Vec<RawFlashAlgorithm>,
        core: Box<dyn Core>,
    ) -> Target {
        Target {
            identifier: TargetIdentifier {
                chip_name: chip.name.clone(),
                flash_algorithm_name: None,
            },
            flash_algorithms,
            core,
            memory_map: chip.memory_map.clone(),
        }
    }
}
