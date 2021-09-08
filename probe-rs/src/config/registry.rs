//! Internal target registry

use super::{Chip, ChipFamily, ChipInfo, Core, Target, TargetDescriptionSource};
use crate::config::CoreType;
use once_cell::sync::Lazy;
use probe_rs_target::{ArmCoreAccessOptions, CoreAccessOptions, RiscvCoreAccessOptions};
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex, TryLockError};

static REGISTRY: Lazy<Arc<Mutex<Registry>>> =
    Lazy::new(|| Arc::new(Mutex::new(Registry::from_builtin_families())));

/// Error type for all errors which occur when working
/// with the internal registry of targets.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The requested chip was not found in the registry.
    #[error("The requested chip '{0}' was not found in the list of known targets.")]
    ChipNotFound(String),
    /// When searching for a chip based on information read from the target,
    /// no matching chip was found in the registry.
    #[error("The connected chip could not automatically be determined.")]
    ChipAutodetectFailed,
    /// A core type contained in a target description is not supported
    /// in probe-rs.
    #[error("The core type '{0}' is not supported in probe-rs.")]
    UnknownCoreType(String),
    /// An IO error which occured when trying to read a target description file.
    #[error("An IO error was encountered")]
    Io(#[from] std::io::Error),
    /// An error occured while deserializing a YAML target description file.
    #[error("Deserializing the yaml encountered an error")]
    Yaml(#[from] serde_yaml::Error),
    /// YAML target description file is logically invalid
    #[error("Invalid YAML target definition")]
    InvalidTarget(#[source] anyhow::Error),
    /// Unable to lock the registry.
    #[error("Unable to lock registry")]
    LockUnavailable,
}

impl<R> From<TryLockError<R>> for RegistryError {
    fn from(_: TryLockError<R>) -> Self {
        RegistryError::LockUnavailable
    }
}

fn add_generic_targets(vec: &mut Vec<ChipFamily>) {
    vec.extend_from_slice(&[
        ChipFamily {
            name: "Generic ARMv6-M".to_owned(),
            manufacturer: None,
            variants: vec![Chip {
                name: "armv6m".to_owned(),
                part: None,
                cores: vec![Core {
                    name: "core".to_owned(),
                    core_type: CoreType::Armv6m,
                    core_access_options: CoreAccessOptions::Arm(ArmCoreAccessOptions {
                        ap: 0,
                        psel: 0,
                    }),
                }],
                memory_map: vec![],
                flash_algorithms: vec![],
            }],
            flash_algorithms: vec![],
            source: TargetDescriptionSource::Generic,
        },
        ChipFamily {
            name: "Generic ARMv7-M".to_owned(),
            manufacturer: None,
            variants: vec![Chip {
                name: "armv7m".to_owned(),
                part: None,
                cores: vec![Core {
                    name: "core".to_owned(),
                    core_type: CoreType::Armv7m,
                    core_access_options: CoreAccessOptions::Arm(ArmCoreAccessOptions {
                        ap: 0,
                        psel: 0,
                    }),
                }],
                memory_map: vec![],
                flash_algorithms: vec![],
            }],
            flash_algorithms: vec![],
            source: TargetDescriptionSource::Generic,
        },
        ChipFamily {
            name: "Generic ARMv8-M".to_owned(),
            manufacturer: None,
            variants: vec![Chip {
                name: "armv8m".to_owned(),
                part: None,
                cores: vec![Core {
                    name: "core".to_owned(),
                    core_type: CoreType::Armv8m,
                    core_access_options: CoreAccessOptions::Arm(ArmCoreAccessOptions {
                        ap: 0,
                        psel: 0,
                    }),
                }],
                memory_map: vec![],
                flash_algorithms: vec![],
            }],
            flash_algorithms: vec![],
            source: TargetDescriptionSource::Generic,
        },
        ChipFamily {
            name: "Generic RISC-V".to_owned(),
            manufacturer: None,
            variants: vec![Chip {
                name: "riscv".to_owned(),
                part: None,
                cores: vec![Core {
                    name: "core".to_owned(),
                    core_type: CoreType::Riscv,
                    core_access_options: CoreAccessOptions::Riscv(RiscvCoreAccessOptions {}),
                }],
                memory_map: vec![],
                flash_algorithms: vec![],
            }],
            flash_algorithms: vec![],
            source: TargetDescriptionSource::Generic,
        },
    ]);
}

/// Registry of all available targets.
struct Registry {
    /// All the available chips.
    families: Vec<ChipFamily>,
}

impl Registry {
    #[cfg(feature = "builtin-targets")]
    fn from_builtin_families() -> Self {
        const BUILTIN_TARGETS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/targets.bincode"));

        let mut families: Vec<ChipFamily> = bincode::deserialize(BUILTIN_TARGETS)
            .expect("Failed to deserialize builtin targets. This is a bug.");

        add_generic_targets(&mut families);

        Self { families }
    }

    #[cfg(not(feature = "builtin-targets"))]
    fn from_builtin_families() -> Self {
        let mut families = vec![];
        add_generic_targets(&mut families);
        Self { families }
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
            let (family, chip) = selected_family_and_chip
                .ok_or_else(|| RegistryError::ChipNotFound(name.to_owned()))?;

            // Try get the correspnding flash algorithm.
            (family, chip)
        };
        self.get_target(family, chip)
    }

    fn search_chips(&self, name: &str) -> Vec<String> {
        log::debug!("Searching registry for chip with name {}", name);

        let mut targets = Vec::new();

        for family in &self.families {
            for variant in family.variants.iter() {
                if variant
                    .name
                    .to_ascii_lowercase()
                    .starts_with(&name.to_ascii_lowercase())
                {
                    targets.push(variant.name.to_string())
                }
            }
        }

        targets
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
        let target =
            Target::new(family, &chip.name).map_err(|e| RegistryError::InvalidTarget(e))?;
        Ok(target)
    }

    fn add_target_from_yaml(&mut self, path_to_yaml: &Path) -> Result<(), RegistryError> {
        let file = File::open(path_to_yaml)?;
        let chip: ChipFamily = serde_yaml::from_reader(file)?;

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

/// Get a target from the internal registry based on its name.
pub fn get_target_by_name(name: impl AsRef<str>) -> Result<Target, RegistryError> {
    REGISTRY.try_lock()?.get_target_by_name(name)
}

/// Get a target from the internal registry based on its name.
pub fn search_chips(name: impl AsRef<str>) -> Result<Vec<String>, RegistryError> {
    Ok(REGISTRY.try_lock()?.search_chips(name.as_ref()))
}

/// Try to retrieve a target based on [ChipInfo] read from a target.
pub(crate) fn get_target_by_chip_info(chip_info: ChipInfo) -> Result<Target, RegistryError> {
    REGISTRY.try_lock()?.get_target_by_chip_info(chip_info)
}

/// Parse a target description file and add the contained targets
/// to the internal target registry.
pub fn add_target_from_yaml(path_to_yaml: &Path) -> Result<(), RegistryError> {
    REGISTRY.try_lock()?.add_target_from_yaml(path_to_yaml)
}

/// Get a list of all families which are contained in the internal
/// registry.
pub fn families() -> Result<Vec<ChipFamily>, RegistryError> {
    Ok(REGISTRY.try_lock()?.families().clone())
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

    #[test]
    fn validate_builtin() {
        let registry = Registry::from_builtin_families();
        for family in registry.families() {
            for chip in &family.variants {
                if let Err(e) = Target::new(family, &chip.name) {
                    panic!(
                        "Failed validating family '{}' chip '{}': {:?}",
                        family.name, chip.name, e
                    );
                }
            }
        }
    }
}
