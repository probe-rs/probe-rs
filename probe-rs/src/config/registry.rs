//! Internal target registry

use super::{Chip, ChipFamily, ChipInfo, Core, Target, TargetDescriptionSource};
use crate::config::CoreType;
use once_cell::sync::Lazy;
use probe_rs_target::{CoreAccessOptions, RiscvCoreAccessOptions};
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex};

static REGISTRY: Lazy<Arc<Mutex<Registry>>> =
    Lazy::new(|| Arc::new(Mutex::new(Registry::from_builtin_families())));

/// Error type for all errors which occur when working
/// with the internal registry of targets.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The requested chip was not found in the registry.
    #[error("The requested chip '{0}' was not found in the list of known targets.")]
    ChipNotFound(String),
    /// Multiple chips found which match the given string, unable to return a single chip.
    #[error("Found multiple chips matching '{0}', unable to select a single chip.")]
    ChipNotUnique(String),
    /// When searching for a chip based on information read from the target,
    /// no matching chip was found in the registry.
    #[error("The connected chip could not automatically be determined.")]
    ChipAutodetectFailed,
    /// A core type contained in a target description is not supported
    /// in probe-rs.
    #[error("The core type '{0}' is not supported in probe-rs.")]
    UnknownCoreType(String),
    /// An IO error which occurred when trying to read a target description file.
    #[error("An IO error was encountered")]
    Io(#[from] std::io::Error),
    /// An error occurred while deserializing a YAML target description file.
    #[error("Deserializing the yaml encountered an error")]
    Yaml(#[from] serde_yaml::Error),
    /// An invalid [`ChipFamily`] was encountered.
    #[error("Invalid chip family definition ({})", .0.name)]
    InvalidChipFamilyDefinition(ChipFamily, String),
}

fn add_generic_targets(vec: &mut Vec<ChipFamily>) {
    vec.extend_from_slice(&[
        ChipFamily {
            name: "Generic ARMv6-M".to_owned(),
            manufacturer: None,
            generated_from_pack: false,
            pack_file_release: None,
            variants: vec![
                Chip::generic_arm("Cortex-M0", CoreType::Armv6m),
                Chip::generic_arm("Cortex-M0+", CoreType::Armv6m),
                Chip::generic_arm("Cortex-M1", CoreType::Armv6m),
            ],

            flash_algorithms: vec![],
            source: TargetDescriptionSource::Generic,
        },
        ChipFamily {
            name: "Generic ARMv7-M".to_owned(),
            manufacturer: None,
            generated_from_pack: false,
            pack_file_release: None,
            variants: vec![Chip::generic_arm("Cortex-M3", CoreType::Armv7m)],
            flash_algorithms: vec![],
            source: TargetDescriptionSource::Generic,
        },
        ChipFamily {
            name: "Generic ARMv7E-M".to_owned(),
            manufacturer: None,
            generated_from_pack: false,
            pack_file_release: None,
            variants: vec![
                Chip::generic_arm("Cortex-M4", CoreType::Armv7em),
                Chip::generic_arm("Cortex-M7", CoreType::Armv7em),
            ],
            flash_algorithms: vec![],
            source: TargetDescriptionSource::Generic,
        },
        ChipFamily {
            name: "Generic ARMv8-M".to_owned(),
            manufacturer: None,
            generated_from_pack: false,
            pack_file_release: None,
            variants: vec![
                Chip::generic_arm("Cortex-M23", CoreType::Armv8m),
                Chip::generic_arm("Cortex-M33", CoreType::Armv8m),
                Chip::generic_arm("Cortex-M35P", CoreType::Armv8m),
                Chip::generic_arm("Cortex-M55", CoreType::Armv8m),
            ],
            flash_algorithms: vec![],
            source: TargetDescriptionSource::Generic,
        },
        ChipFamily {
            name: "Generic RISC-V".to_owned(),
            manufacturer: None,
            pack_file_release: None,
            generated_from_pack: false,
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

        let mut families: Vec<ChipFamily> = match bincode::deserialize(BUILTIN_TARGETS) {
            Ok(families) => families,
            Err(err) => panic!(
                "Failed to deserialize builtin targets. This is a bug : {:?}",
                err
            ),
        };

        add_generic_targets(&mut families);

        // We skip validating the targets here as this is done at a later stage in `get_target`.
        // Additionally, validation for existing targets is done in the tests `validate_generic_targets` and
        // `validate_builtin` as well, to ensure we do not ship broken target definitions.

        Self { families }
    }

    #[cfg(not(feature = "builtin-targets"))]
    fn from_builtin_families() -> Self {
        let mut families = vec![];
        add_generic_targets(&mut families);

        // We skip validating the targets here as this is done at a later stage in `get_target`.
        // Additionally, validation for existing targets is done in the tests `validate_generic_targets` and
        // `validate_builtin` as well, to ensure we do not ship broken target definitions.

        Self { families }
    }

    fn families(&self) -> &Vec<ChipFamily> {
        &self.families
    }

    fn get_target_by_name(&self, name: impl AsRef<str>) -> Result<Target, RegistryError> {
        let name = name.as_ref();

        tracing::debug!("Searching registry for chip with name {}", name);

        let (family, chip) = {
            // Try get the corresponding chip.
            let mut selected_family_and_chip = None;
            let mut exact_matches = 0;
            let mut partial_matches = 0;
            for family in &self.families {
                for variant in family.variants.iter() {
                    if match_name_prefix(&variant.name, name) {
                        if variant.name.len() == name.len() {
                            tracing::debug!("Exact match for chip name: {}", variant.name);
                            exact_matches += 1;
                        } else {
                            tracing::debug!("Partial match for chip name: {}", variant.name);
                            partial_matches += 1;
                            if exact_matches > 0 {
                                continue;
                            }
                        }
                        selected_family_and_chip = Some((family, variant));
                    }
                }
            }
            if exact_matches > 1 || (exact_matches == 0 && partial_matches > 1) {
                tracing::warn!(
                    "Ignoring ambiguous matches for specified chip name {}",
                    name,
                );
                return Err(RegistryError::ChipNotUnique(name.to_owned()));
            }
            let (family, chip) = selected_family_and_chip
                .ok_or_else(|| RegistryError::ChipNotFound(name.to_owned()))?;
            if exact_matches == 0 && partial_matches == 1 {
                tracing::warn!(
                    "Found chip {} which matches given partial name {}. Consider specifying its full name.",
                    chip.name,
                    name,
                );
            }
            if chip.name.to_ascii_lowercase() != name.to_ascii_lowercase() {
                tracing::warn!(
                    "Matching {} based on wildcard. Consider specifying the chip as {} instead.",
                    name,
                    chip.name,
                );
            }

            // Try get the correspnding flash algorithm.
            (family, chip)
        };
        self.get_target(family, chip)
    }

    fn search_chips(&self, name: &str) -> Vec<String> {
        tracing::debug!("Searching registry for chip with name {}", name);

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
                        tracing::debug!("Checking family {}", family.name);

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
                        tracing::debug!(
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
        // The validity of the given `ChipFamily` is checked in the constructor.
        Target::new(family, &chip.name)
    }

    fn add_target_from_yaml(&mut self, path_to_yaml: &Path) -> Result<(), RegistryError> {
        let file = File::open(path_to_yaml)?;
        let family: ChipFamily = serde_yaml::from_reader(file)?;

        family
            .validate()
            .map_err(|e| RegistryError::InvalidChipFamilyDefinition(family.clone(), e))?;

        let index = self
            .families
            .iter()
            .position(|old_family| old_family.name == family.name);
        if let Some(index) = index {
            self.families.remove(index);
        }
        self.families.push(family);

        Ok(())
    }
}

/// Get a target from the internal registry based on its name.
pub fn get_target_by_name(name: impl AsRef<str>) -> Result<Target, RegistryError> {
    REGISTRY.lock().unwrap().get_target_by_name(name)
}

/// Get a target from the internal registry based on its name.
pub fn search_chips(name: impl AsRef<str>) -> Result<Vec<String>, RegistryError> {
    Ok(REGISTRY.lock().unwrap().search_chips(name.as_ref()))
}

/// Try to retrieve a target based on [ChipInfo] read from a target.
pub(crate) fn get_target_by_chip_info(chip_info: ChipInfo) -> Result<Target, RegistryError> {
    REGISTRY.lock().unwrap().get_target_by_chip_info(chip_info)
}

/// Parse a target description file and add the contained targets
/// to the internal target registry.
pub fn add_target_from_yaml(path_to_yaml: &Path) -> Result<(), RegistryError> {
    REGISTRY.lock().unwrap().add_target_from_yaml(path_to_yaml)
}

/// Get a list of all families which are contained in the internal
/// registry.
pub fn families() -> Result<Vec<ChipFamily>, RegistryError> {
    Ok(REGISTRY.lock().unwrap().families().clone())
}

/// See if `name` matches the start of `pattern`, treating any lower-case `x`
/// character in `pattern` as a wildcard that matches any character in `name`.
///
/// Both `name` and `pattern` are compared case-insensitively.
fn match_name_prefix(pattern: &str, name: &str) -> bool {
    // If `name` is shorter than `pattern` but all characters in `name` match,
    // the iterator will end early and the function returns true.
    for (n, p) in name.to_ascii_lowercase().chars().zip(pattern.chars()) {
        if p.to_ascii_lowercase() != n && p != 'x' {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_fetch_not_unique() {
        let registry = Registry::from_builtin_families();
        // ambiguous: partially matches STM32G081KBUx and STM32G081KBUxN
        assert!(matches!(
            registry.get_target_by_name("STM32G081KBU"),
            Err(RegistryError::ChipNotUnique(_))
        ));
    }

    #[test]
    fn try_fetch_not_found() {
        let registry = Registry::from_builtin_families();
        assert!(matches!(
            registry.get_target_by_name("not_a_real_chip"),
            Err(RegistryError::ChipNotFound(_))
        ));
    }

    #[test]
    fn try_fetch2() {
        let registry = Registry::from_builtin_families();
        // ok: matches both STM32G081KBUx and STM32G081KBUxN, but the first one is an exact match
        assert!(registry.get_target_by_name("stm32G081KBUx").is_ok());
    }

    #[test]
    fn try_fetch3() {
        let registry = Registry::from_builtin_families();
        // ok: unique substring match
        assert!(registry.get_target_by_name("STM32G081RBI").is_ok());
    }

    #[test]
    fn try_fetch4() {
        let registry = Registry::from_builtin_families();
        // ok: unique exact match
        assert!(registry.get_target_by_name("nrf51822_Xxaa").is_ok());
    }

    #[test]
    fn validate_generic_targets() {
        let mut families = vec![];
        add_generic_targets(&mut families);

        families
            .iter()
            .map(|family| family.validate())
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
    }

    #[test]
    fn validate_builtin() {
        let registry = Registry::from_builtin_families();
        registry
            .families()
            .iter()
            .map(|family| family.validate())
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
    }
}
