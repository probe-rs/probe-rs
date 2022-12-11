//! Internal target registry

use super::{ChipFamily, Target};
use crate::architecture::arm::ArmChipInfo;
use crate::provider::{Provider, Variant};
use crate::Memory;
use std::path::Path;

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

/// Registry of all available targets.
pub struct Registry {
    /// All the available chips.
    providers: Vec<Box<dyn Provider>>,
}

impl Default for Registry {
    fn default() -> Self {
        let mut providers: Vec<Box<dyn Provider>> = Vec::new();

        providers.push(Box::new(crate::provider::Generic::new()));

        #[cfg(feature = "builtin-targets")]
        providers.push(Box::new(crate::provider::Builtin::new()));

        Self { providers }
    }
}

impl Registry {
    /// Get a target from the internal registry based on its name.
    pub fn get_target_by_name(&self, name: impl AsRef<str>) -> Result<Target, RegistryError> {
        let name = name.as_ref();

        tracing::debug!("Searching registry for chip with name {}", name);

        // Try get the corresponding chip.
        let mut selected = None;
        let mut exact_matches = 0;
        let mut partial_matches = 0;
        for provider in &self.providers {
            for family in provider.families() {
                for variant in family.variants() {
                    let variant_name = variant.name();
                    if match_name_prefix(&variant_name, name) {
                        if variant_name.len() == name.len() {
                            tracing::debug!("Exact match for chip name: {}", variant_name);
                            exact_matches += 1;
                        } else {
                            tracing::debug!("Partial match for chip name: {}", variant_name);
                            partial_matches += 1;
                            if exact_matches > 0 {
                                continue;
                            }
                        }
                        selected = Some(variant);
                    }
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
        let selected = selected.ok_or_else(|| RegistryError::ChipNotFound(name.to_owned()))?;
        if exact_matches == 0 && partial_matches == 1 {
            tracing::warn!(
                    "Found chip {} which matches given partial name {}. Consider specifying its full name.",
                    selected.name(),
                    name,
                );
        }
        if selected.name().to_ascii_lowercase() != name.to_ascii_lowercase() {
            tracing::warn!(
                "Matching {} based on wildcard. Consider specifying the chip as {} instead.",
                name,
                selected.name(),
            );
        }

        Ok(selected.to_target())
    }

    /// Get a target from the internal registry based on its name.
    pub fn search_chips(&self, name: &str) -> Vec<String> {
        tracing::debug!("Searching registry for chip with name {}", name);

        self.providers
            .iter()
            .flat_map(|provider| {
                provider
                    .families()
                    .flat_map(|f| f.variants().map(|t| t.name().to_string()))
            })
            .filter(|variant_name| {
                variant_name
                    .to_ascii_lowercase()
                    .starts_with(&name.to_ascii_lowercase())
            })
            .collect()
    }

    /// Get a list of all families which are contained in the internal registry.
    pub fn families(&self) -> Vec<FamilyRecord> {
        self.providers
            .iter()
            .flat_map(|provider| {
                provider.families().map(|family| FamilyRecord {
                    name: family.name().to_string(),
                    chip_names: family
                        .variants()
                        .map(|variant| variant.name().to_string())
                        .collect(),
                })
            })
            .collect()
    }

    /// Parse a target description file and add the contained targets
    /// to the internal target registry.
    pub fn add_target_from_yaml(&mut self, path_to_yaml: &Path) -> Result<(), RegistryError> {
        // todo: replacement?
        let provider = crate::provider::File::new(path_to_yaml)?;
        self.providers.push(Box::new(provider));
        Ok(())
    }

    pub(crate) fn autodetect_arm(
        &self,
        chip_info: &ArmChipInfo,
        memory: &mut Memory,
    ) -> Option<Box<dyn Variant + '_>> {
        for provider in &self.providers {
            if let Some(variant) = provider.autodetect_arm(chip_info, memory) {
                return Some(variant);
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
pub struct FamilyRecord {
    pub name: String,
    pub chip_names: Vec<String>,
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
        let registry = Registry::default();
        // ambiguous: partially matches STM32G081KBUx and STM32G081KBUxN
        assert!(matches!(
            registry.get_target_by_name("STM32G081KBU"),
            Err(RegistryError::ChipNotUnique(_))
        ));
    }

    #[test]
    fn try_fetch_not_found() {
        let registry = Registry::default();
        assert!(matches!(
            registry.get_target_by_name("not_a_real_chip"),
            Err(RegistryError::ChipNotFound(_))
        ));
    }

    #[test]
    fn try_fetch2() {
        let registry = Registry::default();
        // ok: matches both STM32G081KBUx and STM32G081KBUxN, but the first one is an exact match
        assert!(registry.get_target_by_name("stm32G081KBUx").is_ok());
    }

    #[test]
    fn try_fetch3() {
        let registry = Registry::default();
        // ok: unique substring match
        assert!(registry.get_target_by_name("STM32G081RBI").is_ok());
    }

    #[test]
    fn try_fetch4() {
        let registry = Registry::default();
        // ok: unique exact match
        assert!(registry.get_target_by_name("nrf51822_Xxaa").is_ok());
    }
}
