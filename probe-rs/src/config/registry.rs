use crate::config::{
    chip::Chip,
    flash_algorithm::FlashAlgorithm,
    memory::{FlashRegion, MemoryRegion, RamRegion},
};
use crate::target::info::ChipInfo;
use std::error::Error;
use std::fs::File;
use std::path::Path;

use super::target::Target;
use crate::cores::get_core;

#[derive(Debug)]
pub enum RegistryError {
    ChipNotFound,
    AlgorithmNotFound,
    CoreNotFound,
    Io(std::io::Error),
    Yaml(serde_yaml::Error),
}

impl Error for RegistryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        use RegistryError::*;

        match self {
            ChipNotFound => None,
            AlgorithmNotFound => None,
            CoreNotFound => None,
            Io(ref e) => Some(e),
            Yaml(ref e) => Some(e),
        }
    }
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        use RegistryError::*;

        match self {
            ChipNotFound => write!(f, "The requested chip was not found."),
            AlgorithmNotFound => write!(f, "The requested algorithm was not found."),
            CoreNotFound => write!(f, "The requested core was not found."),
            Io(ref e) => e.fmt(f),
            Yaml(ref e) => e.fmt(f),
        }
    }
}

impl From<std::io::Error> for RegistryError {
    fn from(value: std::io::Error) -> RegistryError {
        RegistryError::Io(value)
    }
}

impl From<serde_yaml::Error> for RegistryError {
    fn from(value: serde_yaml::Error) -> RegistryError {
        RegistryError::Yaml(value)
    }
}

pub enum SelectionStrategy {
    TargetIdentifier(TargetIdentifier),
    ChipInfo(ChipInfo),
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

    pub fn get_target(&self, strategy: SelectionStrategy) -> Result<Target, RegistryError> {
        let (chip, flash_algorithm) = match strategy {
            SelectionStrategy::TargetIdentifier(identifier) => {
                // Try get the corresponding chip.
                let potential_chip = self
                    .chips
                    .iter()
                    .find(|chip| {
                        chip.name
                            .starts_with(&identifier.chip_name.to_ascii_lowercase())
                    })
                    .ok_or_else(|| RegistryError::ChipNotFound)?;

                // Try get the correspnding flash algorithm.
                let potential_flash_algorithm = potential_chip
                    .flash_algorithms
                    .iter()
                    .find(|flash_algorithm| {
                        if let Some(flash_algorithm_name) = identifier.flash_algorithm_name.clone()
                        {
                            flash_algorithm.name == flash_algorithm_name
                        } else {
                            flash_algorithm.default
                        }
                    })
                    .or_else(|| potential_chip.flash_algorithms.first())
                    .ok_or_else(|| RegistryError::AlgorithmNotFound)?;

                (potential_chip, potential_flash_algorithm)
            }
            SelectionStrategy::ChipInfo(chip_info) => {
                // Try get the corresponding chip.
                let potential_chip = self
                    .chips
                    .iter()
                    .find(|chip| {
                        chip.manufacturer
                            .map(|m| m == chip_info.manufacturer)
                            .unwrap_or(false)
                            && chip.part.map(|p| p == chip_info.part).unwrap_or(false)
                    })
                    .ok_or_else(|| RegistryError::ChipNotFound)?;

                // Try get the correspnding flash algorithm.
                let potential_flash_algorithm = potential_chip
                    .flash_algorithms
                    .first()
                    .ok_or_else(|| RegistryError::AlgorithmNotFound)?;

                (potential_chip, potential_flash_algorithm)
            }
        };

        // Try get the corresponding chip.
        let core = if let Some(core) = get_core(&chip.core) {
            core
        } else {
            return Err(RegistryError::CoreNotFound);
        };

        Ok(Target::from((chip, flash_algorithm, core)))
    }

    pub fn add_target_from_yaml(&mut self, path_to_yaml: &Path) -> Result<&Chip, RegistryError> {
        let file = File::open(path_to_yaml)?;
        let chip = Chip::from_yaml_reader(file)?;

        let index = self
            .chips
            .iter()
            .position(|old_chip| old_chip.name == chip.name);
        if let Some(index) = index {
            self.chips.remove(index);
        }
        self.chips.push(chip);

        Ok(self.chips.last().unwrap())
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
