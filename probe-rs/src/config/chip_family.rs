use jep106::JEP106Code;
use super::chip::Chip;
use super::flash_algorithm::RawFlashAlgorithm;
use crate::config::target::TargetParseError;

use serde::{Serialize, Deserialize};

/// This describes a chip family with all its variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipFamily {
    /// This is the name of the chip family in base form.
    /// E.g. `nRF52832`.
    pub name: String,
    /// The JEP106 code of the manufacturer.
    pub manufacturer: Option<JEP106Code>,
    /// The `PART` register of the chip.
    /// This value can be determined via the `cli info` command.
    pub part: Option<u16>,
    /// This vector holds all the variants of the family.
    pub variants: Vec<Chip>,
    /// This vector holds all available algorithms.
    pub flash_algorithms: Vec<RawFlashAlgorithm>,
    /// The name of the core type.
    /// E.g. `M0` or `M4`.
    pub core: String,
}

impl ChipFamily {
    pub fn from_yaml_reader<R: std::io::Read>(
        definition_reader: R,
    ) -> Result<Self, TargetParseError> {
        serde_yaml::from_reader(definition_reader)
    }

    pub fn variants(&self) -> &Vec<Chip> {
        &self.variants
    }

    pub fn algorithms(&self) -> &Vec<RawFlashAlgorithm> {
        &self.flash_algorithms
    }
}
