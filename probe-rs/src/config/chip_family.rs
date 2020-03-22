use super::chip::Chip;
use super::flash_algorithm::RawFlashAlgorithm;
use crate::config::TargetParseError;
use jep106::JEP106Code;
use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// This describes a chip family with all its variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipFamily {
    /// This is the name of the chip family in base form.
    /// E.g. `nRF52832`.
    pub name: Cow<'static, str>,
    /// The JEP106 code of the manufacturer.
    pub manufacturer: Option<JEP106Code>,
    /// This vector holds all the variants of the family.
    pub variants: Cow<'static, [Chip]>,
    /// This vector holds all available algorithms.
    pub flash_algorithms: Cow<'static, [RawFlashAlgorithm]>,
    /// The name of the core type.
    /// E.g. `M0` or `M4`.
    pub core: Cow<'static, str>,
}

impl ChipFamily {
    pub fn from_yaml_reader<R: std::io::Read>(
        definition_reader: R,
    ) -> Result<Self, TargetParseError> {
        serde_yaml::from_reader(definition_reader)
    }

    pub fn variants(&self) -> &[Chip] {
        &self.variants
    }

    pub fn algorithms(&self) -> &[RawFlashAlgorithm] {
        &self.flash_algorithms
    }

    pub fn get_algorithm(&self, name: impl AsRef<str>) -> Option<&RawFlashAlgorithm> {
        let name = name.as_ref();
        self.flash_algorithms.iter().find(|elem| elem.name == name)
    }
}
