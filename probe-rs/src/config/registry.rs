use crate::config::{
    memory::{MemoryRegion, FlashRegion, RamRegion },
    flash_algorithm::FlashAlgorithm,
    chip::Chip,
};

use super::target::Target;
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
    chips: Vec<Chip>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            chips: include!(concat!(env!("OUT_DIR"), "/targets.rs")),
        }
    }

    pub fn get_target(&self, identifier: TargetIdentifier) -> Result<Target, TargetSelectionError> {
        // Try get the corresponding chip.
        let potential_chip = self.chips.iter().find(|chip| {
            chip.name.starts_with(&identifier.chip_name.to_ascii_lowercase())
        });

        // Check if a corresponding chip was found.
        let chip = if let Some(chip) = potential_chip {
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

impl<S: AsRef<str>> From<S> for TargetIdentifier {
    fn from(value: S) -> TargetIdentifier {
        let split: Vec<_> = value.as_ref().split("::").collect();
        TargetIdentifier {
            // There will always be a 0th element, so this is safe!
            chip_name: split[0].to_owned(),
            flash_algorithm_name: split.get(1).map(|s| s.to_owned().to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_fetch1() {
        let registry = Registry::new();
        assert!(registry.get_target("nrf51".into()).is_ok());
    }

    #[test]
    fn try_fetch2() {
        let registry = Registry::new();
        assert!(registry.get_target("nrf5182".into()).is_ok());
    }

    #[test]
    fn try_fetch3() {
        let registry = Registry::new();
        assert!(registry.get_target("nrF51822_x".into()).is_ok());
    }

    #[test]
    fn try_fetch4() {
        let registry = Registry::new();
        assert!(registry.get_target("nrf51822_Xxaa".into()).is_ok());
    }
}