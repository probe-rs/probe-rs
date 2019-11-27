use super::chip::Chip;
use super::chip_family::ChipFamily;
use super::flash_algorithm::{FlashAlgorithm, RawFlashAlgorithm};
use super::memory::{FlashRegion, MemoryRegion, RamRegion};
use super::registry::TargetIdentifier;
use crate::target::Core;
use jep106::JEP106Code;

/// This describes a complete target with a fixed chip model and variant.
#[derive(Debug, Clone)]
pub struct Target {
    /// The complete identifier of the target.
    pub identifier: TargetIdentifier,
    /// The JEP106 code of the manufacturer.
    pub manufacturer: Option<JEP106Code>,
    /// The `PART` register of the chip.
    /// This value can be determined via the `cli info command`.
    pub part: Option<u16>,
    /// The name of the flash algorithm.
    pub flash_algorithm: Option<FlashAlgorithm>,
    /// The core type.
    pub core: Box<dyn Core>,
    /// The memory map of the target.
    pub memory_map: Vec<MemoryRegion>,
}

pub type TargetParseError = serde_yaml::Error;

impl
    From<(
        &ChipFamily,
        &Chip,
        &RamRegion,
        &FlashRegion,
        &RawFlashAlgorithm,
        Box<dyn Core>,
    )> for Target
{
    fn from(
        value: (
            &ChipFamily,
            &Chip,
            &RamRegion,
            &FlashRegion,
            &RawFlashAlgorithm,
            Box<dyn Core>,
        ),
    ) -> Target {
        let (chip_family, chip, ram, flash, flash_algorithm, core) = value;

        Target {
            identifier: TargetIdentifier {
                chip_name: chip.name.clone(),
                flash_algorithm_name: Some(flash_algorithm.name.clone()),
            },
            manufacturer: chip_family.manufacturer,
            part: chip_family.part,
            flash_algorithm: Some(flash_algorithm.assemble(ram, flash)),
            core,
            memory_map: chip.memory_map.clone(),
        }
    }
}
