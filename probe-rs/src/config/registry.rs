use std::collections::HashMap;
use std::path::Path;

use super::target::Target;
use super::chip::Chip;
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

        // Try get the correspnding flash algorithm.
        // TODO: fix algo selection (should take default)
        let potential_flash_algorithm = chip.flash_algorithms.iter().find(|flash_algorithm| {
            if let Some(flash_algorithm_name) = identifier.flash_algorithm_name.clone() {
                flash_algorithm.name == flash_algorithm_name
            } else {
                flash_algorithm.default
            }
        }).or_else(|| chip.flash_algorithms.first());

        let flash_algorithm = if let Some(flash_algorithm) = potential_flash_algorithm {
            flash_algorithm
        } else {
            return Err(TargetSelectionError::AlgorithmNotFound);
        };

        Ok(Target::from((chip, flash_algorithm, core)))
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetIdentifier {
    pub chip_name: String,
    pub flash_algorithm_name: Option<String>,
}

impl From<String> for TargetIdentifier {
    fn from(value: String) -> TargetIdentifier {

        let split: Vec<_> = value.split("::").collect();
        TargetIdentifier {
            // There will always be a 0th element, so this is safe!
            chip_name: split[0].to_owned(),
            flash_algorithm_name: split.get(1).map(|s| s.to_owned().to_owned()),
        }
    }
}