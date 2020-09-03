use super::target::Target;
use crate::config::{Chip, ChipFamily, ChipInfo};
use crate::core::CoreType;
use lazy_static::lazy_static;
use std::fs::File;
use std::path::Path;
use std::{
    borrow::Cow,
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
    #[error("An IO error was encountered")]
    Io(#[from] std::io::Error),
    #[error("Deserializing the yaml encountered an error")]
    Yaml(#[from] serde_yaml::Error),
    #[error("Unable to lock registry")]
    LockUnavailable,
}

impl<R> From<TryLockError<R>> for RegistryError {
    fn from(_: TryLockError<R>) -> Self {
        RegistryError::LockUnavailable
    }
}

const GENERIC_TARGETS: [ChipFamily; 6] = [
    ChipFamily {
        name: Cow::Borrowed("Generic Cortex-M0"),
        manufacturer: None,
        variants: Cow::Borrowed(&[Chip {
            name: Cow::Borrowed("cortex-m0"),
            part: None,
            memory_map: Cow::Borrowed(&[]),
            flash_algorithms: Cow::Borrowed(&[]),
        }]),
        flash_algorithms: Cow::Borrowed(&[]),
        core: Cow::Borrowed("M0"),
    },
    ChipFamily {
        name: Cow::Borrowed("Generic Cortex-M4"),
        manufacturer: None,
        variants: Cow::Borrowed(&[Chip {
            name: Cow::Borrowed("cortex-m4"),
            part: None,
            memory_map: Cow::Borrowed(&[]),
            flash_algorithms: Cow::Borrowed(&[]),
        }]),
        flash_algorithms: Cow::Borrowed(&[]),
        core: Cow::Borrowed("M4"),
    },
    ChipFamily {
        name: Cow::Borrowed("Generic Cortex-M3"),
        manufacturer: None,
        variants: Cow::Borrowed(&[Chip {
            name: Cow::Borrowed("cortex-m3"),
            part: None,
            memory_map: Cow::Borrowed(&[]),
            flash_algorithms: Cow::Borrowed(&[]),
        }]),
        flash_algorithms: Cow::Borrowed(&[]),
        core: Cow::Borrowed("M3"),
    },
    ChipFamily {
        name: Cow::Borrowed("Generic Cortex-M33"),
        manufacturer: None,
        variants: Cow::Borrowed(&[Chip {
            name: Cow::Borrowed("cortex-m33"),
            part: None,
            memory_map: Cow::Borrowed(&[]),
            flash_algorithms: Cow::Borrowed(&[]),
        }]),
        flash_algorithms: Cow::Borrowed(&[]),
        core: Cow::Borrowed("M33"),
    },
    ChipFamily {
        name: Cow::Borrowed("Generic Cortex-M7"),
        manufacturer: None,
        variants: Cow::Borrowed(&[Chip {
            name: Cow::Borrowed("cortex-m7"),
            part: None,
            memory_map: Cow::Borrowed(&[]),
            flash_algorithms: Cow::Borrowed(&[]),
        }]),
        flash_algorithms: Cow::Borrowed(&[]),
        core: Cow::Borrowed("M7"),
    },
    ChipFamily {
        name: Cow::Borrowed("Generic Riscv"),
        manufacturer: None,
        variants: Cow::Borrowed(&[Chip {
            name: Cow::Borrowed("riscv"),
            part: None,
            memory_map: Cow::Borrowed(&[]),
            flash_algorithms: Cow::Borrowed(&[]),
        }]),
        flash_algorithms: Cow::Borrowed(&[]),
        core: Cow::Borrowed("riscv"),
    },
];

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
        let mut families = Vec::from(builtin::get_targets());

        families.extend(GENERIC_TARGETS.iter().cloned());

        Self { families }
    }

    #[cfg(not(feature = "builtin-targets"))]
    fn from_builtin_families() -> Self {
        Self {
            families: GENERIC_TARGETS.iter().cloned().collect(),
        }
    }

    fn families(&self) -> &Vec<ChipFamily> {
        &self.families
    }

    fn get_target_by_name(&self, name: impl AsRef<str>) -> Result<Target, RegistryError> {
        let name = name.as_ref();

        log::debug!("Searching registry for chip with name {}", name);

        let (family, chip) = {
            // Try get the corresponding chip.
            let mut selected_family_and_chip = None;
            for family in &self.families {
                for variant in family.variants.iter() {
                    if variant
                        .name
                        .to_ascii_lowercase()
                        .starts_with(&name.to_ascii_lowercase())
                    {
                        if variant.name.to_ascii_lowercase() != name.to_ascii_lowercase() {
                            log::warn!(
                                "Found chip {} which matches given partial name {}. Consider specifying its full name.",
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

                    let families = self.families.iter().filter(|f| {
                        f.manufacturer
                            .map(|m| m == chip_info.manufacturer)
                            .unwrap_or(false)
                    });

                    let mut identified_chips = Vec::new();

                    for family in families {
                        log::debug!("Checking family {}", family.name);

                        let chips = family
                            .variants()
                            .iter()
                            .filter(|v| v.part.map(|p| p == chip_info.part).unwrap_or(false))
                            .map(|c| (family, c));

                        identified_chips.extend(chips)
                    }

                    if identified_chips.len() == 1 {
                        identified_chips.pop().unwrap()
                    } else {
                        log::debug!(
                        "Found {} matching chips for information {:?}, unable to determine chip",
                        identified_chips.len(),
                        chip_info
                    );
                        return Err(RegistryError::ChipAutodetectFailed);
                    }
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
            return Err(RegistryError::CoreNotFound(
                family.core.clone().into_owned(),
            ));
        };

        // find relevant algorithms
        let chip_algorithms = chip
            .flash_algorithms
            .iter()
            .filter_map(|fa| family.get_algorithm(fa))
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
