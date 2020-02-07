use crate::config::chip::Chip;
use crate::config::chip_family::ChipFamily;
use crate::config::chip_info::ChipInfo;
use thiserror::Error;
use std::fs::File;
use std::path::Path;
use super::target::Target;
use crate::core::get_core;
use std::sync::{Arc, Mutex, TryLockError};

lazy_static::lazy_static! {
    static ref REGISTRY: Arc<Mutex<Registry>> = Arc::new(Mutex::new(Registry::from_builtin_families()));
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("The requested chip was not found.")]
    ChipNotFound,
    #[error("The connected chip could not automatically be determined.")]
    ChipAutodetectFailed,
    #[error("The requested algorithm was not found.")]
    AlgorithmNotFound,
    #[error("The requested core was not found.")]
    CoreNotFound,
    #[error("No RAM description was found.")]
    RamMissing,
    #[error("No flash description was found.")]
    FlashMissing,
    #[error("An IO error was encountered: {0}")]
    Io(#[from] std::io::Error),
    #[error("Deserializing the yaml encountered an error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("Unable to lock registry")]
    LockUnavailable,
}

impl<R> From<TryLockError<R>> for RegistryError {
    fn from(_: TryLockError<R>) -> Self {
        RegistryError::LockUnavailable
    }
}

pub struct Registry {
    /// All the available chips.
    families: Vec<ChipFamily>,
}

#[cfg(feature = "builtin-targets")]
mod builtin {
    include!(concat!(env!("OUT_DIR"), "/targets.rs"));
}

impl Registry {
    #[cfg(feature = "builtin-targets")]
    pub fn from_builtin_families() -> Self {
        Self {
            families: builtin::get_targets(),
        }
    }

    #[cfg(not(feature = "builtin-targets"))]
    pub fn from_builtin_families() -> Self {
        Self {
            families: Vec::new(),
        }
    }

    pub fn families(&self) -> &Vec<ChipFamily> {
        &self.families
    }

    pub fn get_target_by_name(&self, name: impl AsRef<str>) -> Result<Target, RegistryError> {
        let (family, chip) = {
            // Try get the corresponding chip.
            let mut selected_family_and_chip = None;
            for family in &self.families {
                for variant in &family.variants {
                    if variant
                        .name
                        .to_ascii_lowercase()
                        .starts_with(&name.as_ref().to_ascii_lowercase())
                    {
                        if variant.name.to_ascii_lowercase() != name.as_ref().to_ascii_lowercase() {
                            log::warn!(
                                "Found chip {} which matches given partial name {}. Consider specifying it's full name.",
                                variant.name,
                                name.as_ref(),
                            )
                        }
                        selected_family_and_chip = Some((family, variant));
                    }
                }
            }
            let (family, chip) = selected_family_and_chip.ok_or(RegistryError::ChipNotFound)?;

            // Try get the correspnding flash algorithm.
            (family, chip)
        };
        self.get_target(family, chip)
    }

    pub fn get_target_by_chip_info(&self, chip_info: ChipInfo) -> Result<Target, RegistryError> {
        let (family, chip) = {
            match chip_info {
                ChipInfo::Arm(chip_info) => {
                    // Try get the corresponding chip.
                    let mut selected_family_and_chip = None;
                    for family in &self.families {
                        if family
                            .manufacturer
                            .map(|m| m == chip_info.manufacturer)
                            .unwrap_or(false)
                        {
                            for variant in &family.variants {
                                if variant.part.map(|p| p == chip_info.part).unwrap_or(false) {
                                    selected_family_and_chip = Some((family, variant));
                                }
                            }
                        }
                    }
                    let (family, chip) =
                        selected_family_and_chip.ok_or(RegistryError::ChipAutodetectFailed)?;

                    (family, chip)
                }
            }
        };
        self.get_target(family, chip)
    }

    fn get_target(&self, family: &ChipFamily, chip: &Chip) -> Result<Target, RegistryError> {
        // Try get the corresponding chip.
        let core = if let Some(core) = get_core(&family.core) {
            core
        } else {
            return Err(RegistryError::CoreNotFound);
        };

        // find relevant algorithms
        let chip_algorithms = chip
            .flash_algorithms
            .iter()
            .filter_map(|fa| family.flash_algorithms.get(fa))
            .cloned()
            .collect();

        Ok(Target::new(chip, chip_algorithms, core))
    }

    pub fn add_target_from_yaml(&mut self, path_to_yaml: &Path) -> Result<(), RegistryError> {
        let file = File::open(path_to_yaml)?;
        let chip = ChipFamily::from_yaml_reader(file)?;

        let index = self
            .families
            .iter()
            .position(|old_chip| old_chip.name == chip.name);
        if let Some(index) = index {
            self.families.remove(index);
        }
        self.families.push(chip);

        Ok(())
    }
}

pub fn get_target_by_name(name: impl AsRef<str>) -> Result<Target, RegistryError> {
    REGISTRY.try_lock()?.get_target_by_name(name)
}

pub fn get_target_by_chip_info(chip_info: ChipInfo) -> Result<Target, RegistryError> {
    REGISTRY.try_lock()?.get_target_by_chip_info(chip_info)
}

pub fn add_target_from_yaml(path_to_yaml: &Path) -> Result<(), RegistryError> {
    REGISTRY.try_lock()?.add_target_from_yaml(path_to_yaml)
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetIdentifier {
    pub chip_name: String,
}

impl<S: AsRef<str>> From<S> for TargetIdentifier {
    fn from(value: S) -> TargetIdentifier {
        let split: Vec<_> = value.as_ref().split("::").collect();
        TargetIdentifier {
            // There will always be a 0th element, so this is safe!
            chip_name: split[0].to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_fetch1() {
        let registry = Registry::from_builtin_families();
        assert!(registry.get_target_by_name("nrf51").is_ok());
    }

    #[test]
    fn try_fetch2() {
        let registry = Registry::from_builtin_families();
        assert!(registry.get_target_by_name("nrf5182").is_ok());
    }

    #[test]
    fn try_fetch3() {
        let registry = Registry::from_builtin_families();
        assert!(registry.get_target_by_name("nrF51822_x").is_ok());
    }

    #[test]
    fn try_fetch4() {
        let registry = Registry::from_builtin_families();
        assert!(registry.get_target_by_name("nrf51822_Xxaa").is_ok());
    }
}
