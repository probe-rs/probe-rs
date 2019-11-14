use std::collections::HashMap;
use std::path::Path;

use super::target::Target;
use super::chip::Chip;
use super::flash_algorithm::FlashAlgorithm;
use crate::collection::get_core;

pub enum TargetSelectionError {
    ChipNotFound,
    VariantNotFound,
    AlgorithmNotFound,
    CoreNotFound,
}

pub struct Registry {
    /// All the available chips.
    /// <chip_name, chip>
    chips: HashMap<String, Chip>,
    flash_algorithms: HashMap<String, FlashAlgorithm>,
}

impl Registry {
    pub fn load_from_dir(path: &Path) -> Registry {
        unimplemented!();
    }

    pub fn get_target(&self, identifier: TargetIdentifier) -> Result<Target, TargetSelectionError> {
        // Try get the corresponding chip.
        let chip = if let Some(chip) = self.chips.get(&identifier.chip_name) {
            chip
        } else {
            return Err(TargetSelectionError::ChipNotFound);
        };

        // Try get the corresponding chip.
        let core = if let Some(core) = get_core(&chip.core) {
            core
        } else {
            return Err(TargetSelectionError::CoreNotFound);
        };

        // Determine potential variant.
        let potential_variant = if let Some(variant_name) = identifier.variant_name {
            chip.variants.iter().find(|variant| variant.name == variant_name)
        } else {
            chip.variants.first()
        };

        // Try get the corresponding variant.
        let variant = if let Some(variant) = potential_variant {
            variant
        } else {
            return Err(TargetSelectionError::VariantNotFound);
        };

        // Try get the correspnding flash algorithm.
        let flash_algorithm = if let Some(flash_algorithm) = self.flash_algorithms.get(&chip.flash_algorithm) {
            flash_algorithm
        } else {
            return Err(TargetSelectionError::AlgorithmNotFound);
        };

        Ok(Target::from((chip, variant, flash_algorithm, core)))
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetIdentifier {
    pub chip_name: String,
    pub variant_name: Option<String>,
}

impl From<String> for TargetIdentifier {
    fn from(value: String) -> TargetIdentifier {
        let split: Vec<_> = value.split("::").collect();

        TargetIdentifier {
            // There will always be a 0th element, so this is safe!
            chip_name: split[0].to_owned(),
            variant_name: split.get(1).map(|s| s.to_owned().to_owned()),
        }
    }
}