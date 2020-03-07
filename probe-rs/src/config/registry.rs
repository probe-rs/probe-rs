use super::target::Target;
use crate::config::{Chip, ChipFamily, ChipInfo};
use crate::core::CoreType;
use lazy_static::lazy_static;
use std::fs::File;
use std::path::Path;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, TryLockError},
};
use thiserror::Error;

lazy_static! {
    static ref REGISTRY: Arc<Mutex<Registry>> =
        Arc::new(Mutex::new(Registry::from_builtin_families()));
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("The requested chip was not found.")]
    ChipNotFound,
    #[error("The connected chip could not automatically be determined.")]
    ChipAutodetectFailed,
    #[error("The requested algorithm was not found.")]
    AlgorithmNotFound,
    #[error("The requested core '{0}' was not found.")]
    CoreNotFound(String),
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

lazy_static! {
    static ref GENERIC_TARGETS: [ChipFamily; 5] = [
        ChipFamily {
            name: "Generic Cortex-M0".to_owned(),
            manufacturer: None,
            variants: vec![Chip {
                name: "cortex-m0".into(),
                part: None,
                memory_map: vec![],
                flash_algorithms: vec![],
            }],
            flash_algorithms: HashMap::new(),
            core: "M0".to_owned(),
        },
        ChipFamily {
            name: "Generic Cortex-M4".to_owned(),
            manufacturer: None,
            variants: vec![Chip {
                name: "cortex-m4".to_owned(),
                part: None,
                memory_map: vec![],
                flash_algorithms: vec![],
            }],
            flash_algorithms: HashMap::new(),
            core: "M4".to_owned(),
        },
        ChipFamily {
            name: "Generic Cortex-M3".to_owned(),
            manufacturer: None,
            variants: vec![Chip {
                name: "cortex-m3".to_owned(),
                part: None,
                memory_map: vec![],
                flash_algorithms: vec![],
            }],
            flash_algorithms: HashMap::new(),
            core: "M3".to_owned(),
        },
        ChipFamily {
            name: "Generic Cortex-M33".to_owned(),
            manufacturer: None,
            variants: vec![Chip {
                name: "cortex-m33".to_owned(),
                part: None,
                memory_map: vec![],
                flash_algorithms: vec![],
            }],
            flash_algorithms: HashMap::new(),
            core: "M33".to_owned(),
        },
        ChipFamily {
            name: "Generic Riscv".to_owned(),
            manufacturer: None,
            variants: vec![Chip {
                name: "riscv".to_owned(),
                part: None,
                memory_map: vec![],
                flash_algorithms: vec![],
            }],
            flash_algorithms: HashMap::new(),
            core: "riscv".to_owned(),
        },
    ];
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
    fn from_builtin_families() -> Self {
        let mut families = builtin::get_targets();

        families.extend(GENERIC_TARGETS.iter().cloned());

        Self { families }
    }

    #[cfg(not(feature = "builtin-targets"))]
    fn from_builtin_families() -> Self {
        Self {
            families: Vec::from(generic_targets),
        }
    }

    fn families(&self) -> &Vec<ChipFamily> {
        &self.families
    }

    fn get_target_by_name(&self, name: impl AsRef<str>) -> Result<Target, RegistryError> {
        let name = name.as_ref();

        log::trace!("Searching registry for chip with name {}", name);

        let (family, chip) = {
            // Try get the corresponding chip.
            let mut selected_family_and_chip = None;
            for family in &self.families {
                for variant in &family.variants {
                    if variant
                        .name
                        .to_ascii_lowercase()
                        .starts_with(&name.to_ascii_lowercase())
                    {
                        if variant.name.to_ascii_lowercase() != name.to_ascii_lowercase() {
                            log::warn!(
                                "Found chip {} which matches given partial name {}. Consider specifying it's full name.",
                                variant.name,
                                name,
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

    fn get_target_by_chip_info(&self, chip_info: ChipInfo) -> Result<Target, RegistryError> {
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
        let core = if let Some(core) = CoreType::from_string(&family.core) {
            core
        } else {
            return Err(RegistryError::CoreNotFound(family.core.clone()));
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

    fn add_target_from_yaml(&mut self, path_to_yaml: &Path) -> Result<(), RegistryError> {
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

pub fn families() -> Result<Vec<ChipFamily>, RegistryError> {
    Ok(REGISTRY.try_lock()?.families().clone())
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
