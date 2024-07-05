//! Internal target registry

use super::{Chip, ChipFamily, ChipInfo, Core, Target, TargetDescriptionSource};
use crate::config::CoreType;
use once_cell::sync::Lazy;
use parking_lot::{RwLock, RwLockReadGuard};
use probe_rs_target::{CoreAccessOptions, RiscvCoreAccessOptions};
use std::collections::HashMap;
use std::io::Read;
use std::ops::Deref;

static REGISTRY: Lazy<RwLock<Registry>> =
    Lazy::new(|| RwLock::new(Registry::from_builtin_families()));

/// Error type for all errors which occur when working
/// with the internal registry of targets.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum RegistryError {
    /// The requested chip '{0}' was not found in the list of known targets.
    ChipNotFound(String),
    /// Found multiple chips matching '{0}', unable to select a single chip. ({1})
    ChipNotUnique(String, String),
    /// The connected chip could not automatically be determined.
    ChipAutodetectFailed,
    /// The core type '{0}' is not supported in probe-rs.
    UnknownCoreType(String),
    /// An IO error occurred when trying to read a target description file.
    Io(#[from] std::io::Error),
    /// An error occurred while deserializing a YAML target description file.
    Yaml(#[from] serde_yaml::Error),
    /// Invalid chip family definition ({0.name}): {1}
    InvalidChipFamilyDefinition(Box<ChipFamily>, String),
}

fn add_generic_targets(vec: &mut Vec<ChipFamily>) {
    vec.extend_from_slice(&[
        ChipFamily {
            name: "Generic ARMv6-M".to_owned(),
            manufacturer: None,
            generated_from_pack: false,
            pack_file_release: None,
            chip_detection: vec![],
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
            chip_detection: vec![],
            variants: vec![Chip::generic_arm("Cortex-M3", CoreType::Armv7m)],
            flash_algorithms: vec![],
            source: TargetDescriptionSource::Generic,
        },
        ChipFamily {
            name: "Generic ARMv7E-M".to_owned(),
            manufacturer: None,
            generated_from_pack: false,
            pack_file_release: None,
            chip_detection: vec![],
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
            chip_detection: vec![],
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
            chip_detection: vec![],
            variants: vec![Chip {
                name: "riscv".to_owned(),
                part: None,
                svd: None,
                documentation: HashMap::new(),
                cores: vec![Core {
                    name: "core".to_owned(),
                    core_type: CoreType::Riscv,
                    core_access_options: CoreAccessOptions::Riscv(RiscvCoreAccessOptions {
                        hart_id: None,
                        jtag_tap: None,
                    }),
                }],
                memory_map: vec![],
                flash_algorithms: vec![],
                rtt_scan_ranges: None,
                jtag: None,
                default_binary_format: None,
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

#[cfg(feature = "builtin-targets")]
fn builtin_targets() -> Vec<ChipFamily> {
    const BUILTIN_TARGETS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/targets.bincode"));

    bincode::deserialize(BUILTIN_TARGETS)
        .expect("Failed to deserialize builtin targets. This is a bug")
}

#[cfg(not(feature = "builtin-targets"))]
fn builtin_targets() -> Vec<ChipFamily> {
    vec![]
}

impl Registry {
    fn from_builtin_families() -> Self {
        let mut families = builtin_targets();

        add_generic_targets(&mut families);

        // We skip validating the targets here as this is done at a later stage in `get_target`.
        // Additionally, validation for existing targets is done in the tests `validate_generic_targets` and
        // `validate_builtin` as well, to ensure we do not ship broken target definitions.

        Self { families }
    }

    fn get_target_by_name(&self, name: impl AsRef<str>) -> Result<Target, RegistryError> {
        let (target, _) = self.get_target_and_family_by_name(name.as_ref())?;
        Ok(target)
    }

    fn get_target_and_family_by_name(
        &self,
        name: &str,
    ) -> Result<(Target, ChipFamily), RegistryError> {
        tracing::debug!("Searching registry for chip with name {}", name);

        // Try get the corresponding chip.
        let mut selected_family_and_chip = None;
        let mut exact_matches = 0;
        let mut partial_matches = Vec::new();
        for family in self.families.iter() {
            for variant in family.variants.iter() {
                if match_name_prefix(&variant.name, name) {
                    if variant.name.len() == name.len() {
                        tracing::debug!("Exact match for chip name: {}", variant.name);
                        exact_matches += 1;
                    } else {
                        tracing::debug!("Partial match for chip name: {}", variant.name);
                        partial_matches.push(variant.name.as_str());
                        // Only select partial match if we don't have an exact match yet
                        if exact_matches > 0 {
                            continue;
                        }
                    }
                    selected_family_and_chip = Some((family, variant));
                }
            }
        }

        let Some((family, chip)) = selected_family_and_chip else {
            return Err(RegistryError::ChipNotFound(name.to_string()));
        };

        if exact_matches == 0 {
            match partial_matches.len() {
                0 => {}
                1 => {
                    tracing::warn!(
                        "Found chip {} which matches given partial name {}. Consider specifying its full name.",
                        chip.name,
                        name,
                    );
                }
                matches => {
                    const MAX_PRINTED_MATCHES: usize = 100;
                    tracing::warn!(
                        "Ignoring {matches} ambiguous matches for specified chip name {name}"
                    );

                    let (print, overflow) =
                        partial_matches.split_at(MAX_PRINTED_MATCHES.min(matches));

                    let mut suggestions = print.join(", ");

                    // Avoid "and 1 more" by printing the last item.
                    match overflow.len() {
                        0 => {}
                        1 => suggestions.push_str(&format!(", {}", overflow[0])),
                        _ => suggestions.push_str(&format!("and {} more", overflow.len())),
                    }

                    return Err(RegistryError::ChipNotUnique(name.to_string(), suggestions));
                }
            }
        }

        if !chip.name.eq_ignore_ascii_case(name) {
            tracing::warn!(
                "Matching {} based on wildcard. Consider specifying the chip as {} instead.",
                name,
                chip.name,
            );
        }

        let targ = self.get_target(family, chip);
        Ok((targ, family.clone()))
    }

    fn get_targets_by_family_name(&self, name: &str) -> Result<Vec<String>, RegistryError> {
        let mut found_family = None;
        let mut exact_matches = 0;
        for family in self.families.iter() {
            if match_name_prefix(&family.name, name) {
                if family.name.len() == name.len() {
                    tracing::debug!("Exact match for family name: {}", family.name);
                    exact_matches += 1;
                } else {
                    tracing::debug!("Partial match for family name: {}", family.name);
                    if exact_matches > 0 {
                        continue;
                    }
                }
                found_family = Some(family);
            }
        }
        let Some(family) = found_family else {
            return Err(RegistryError::ChipNotFound(name.to_string()));
        };

        Ok(family.variants.iter().map(|v| v.name.to_string()).collect())
    }

    fn search_chips(&self, name: &str) -> Vec<String> {
        tracing::debug!("Searching registry for chip with name {}", name);

        let mut targets = Vec::new();

        for family in &self.families {
            for variant in family.variants.iter() {
                if match_name_prefix(name, &variant.name) {
                    targets.push(variant.name.to_string());
                }
            }
        }

        targets
    }

    fn get_target_by_chip_info(&self, chip_info: ChipInfo) -> Result<Target, RegistryError> {
        let (family, chip) = match chip_info {
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

                if identified_chips.len() != 1 {
                    tracing::debug!(
                        "Found {} matching chips for information {:?}, unable to determine chip",
                        identified_chips.len(),
                        chip_info
                    );
                    return Err(RegistryError::ChipAutodetectFailed);
                }

                identified_chips[0]
            }
        };
        Ok(self.get_target(family, chip))
    }

    fn get_target(&self, family: &ChipFamily, chip: &Chip) -> Target {
        // The validity of the given `ChipFamily` is checked in test time and in `add_target_from_yaml`.
        Target::new(family, chip)
    }

    fn add_target_from_yaml<R>(&mut self, yaml_reader: R) -> Result<String, RegistryError>
    where
        R: Read,
    {
        let family: ChipFamily = serde_yaml::from_reader(yaml_reader)?;

        validate_family(&family).map_err(|error| {
            RegistryError::InvalidChipFamilyDefinition(Box::new(family.clone()), error)
        })?;

        let family_name = family.name.clone();

        self.families
            .retain(|old_family| !old_family.name.eq_ignore_ascii_case(&family_name));

        self.families.push(family);

        Ok(family_name)
    }
}

/// Get a target from the internal registry based on its name.
pub fn get_target_by_name(name: impl AsRef<str>) -> Result<Target, RegistryError> {
    REGISTRY.read_recursive().get_target_by_name(name)
}

/// Get a target & chip family from the internal registry based on its name.
pub fn get_target_and_family_by_name(
    name: impl AsRef<str>,
) -> Result<(Target, ChipFamily), RegistryError> {
    REGISTRY
        .read_recursive()
        .get_target_and_family_by_name(name.as_ref())
}

/// Get all target from the internal registry based on its family name.
pub fn get_targets_by_family_name(
    family_name: impl AsRef<str>,
) -> Result<Vec<String>, RegistryError> {
    REGISTRY
        .read_recursive()
        .get_targets_by_family_name(family_name.as_ref())
}

/// Returns targets from the internal registry that match the given name.
pub fn search_chips(name: impl AsRef<str>) -> Result<Vec<String>, RegistryError> {
    Ok(REGISTRY.read_recursive().search_chips(name.as_ref()))
}

/// Try to retrieve a target based on [ChipInfo] read from a target.
pub(crate) fn get_target_by_chip_info(chip_info: ChipInfo) -> Result<Target, RegistryError> {
    REGISTRY.read_recursive().get_target_by_chip_info(chip_info)
}

/// Parse a target description and add the contained targets
/// to the internal target registry.
///
/// # Examples
///
/// ## Add targets from a YAML file
///
/// ```no_run
/// use std::path::Path;
/// use std::fs::File;
///
/// let file = File::open(Path::new("/path/target.yaml"))?;
/// probe_rs::config::add_target_from_yaml(file)?;
///
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// ## Add targets from a embedded YAML file
///
/// ```ignore
/// const BUILTIN_TARGET_YAML: &[u8] = include_bytes!("/path/target.yaml");
/// probe_rs::config::add_target_from_yaml(BUILTIN_TARGET_YAML)?;
/// ```
pub fn add_target_from_yaml<R>(yaml_reader: R) -> Result<String, RegistryError>
where
    R: Read,
{
    REGISTRY.write().add_target_from_yaml(yaml_reader)
}

/// Get a list of all families which are contained in the internal
/// registry.
///
/// As opposed to `families()` this function does not clone the families, but using it is
/// slightly more cumbersome.
pub fn families_ref() -> impl Deref<Target = [ChipFamily]> {
    RwLockReadGuard::map(REGISTRY.read_recursive(), |registry| {
        registry.families.as_slice()
    })
}

/// Get a list of all families which are contained in the internal
/// registry.
pub fn families() -> Vec<ChipFamily> {
    families_ref().to_vec()
}

/// See if `name` matches the start of `pattern`, treating any lower-case `x`
/// character in `pattern` as a wildcard that matches any character in `name`.
///
/// Both `name` and `pattern` are compared case-insensitively.
fn match_name_prefix(pattern: &str, name: &str) -> bool {
    // If `name` is shorter than `pattern` but all characters in `name` match,
    // the iterator will end early and the function returns true.
    for (n, p) in name.chars().zip(pattern.chars()) {
        if !n.eq_ignore_ascii_case(&p) && p != 'x' {
            return false;
        }
    }
    true
}

fn validate_family(family: &ChipFamily) -> Result<(), String> {
    family.validate()?;

    // We can't have this in the `validate` method as we need information that is not available in
    // probe-rs-target.
    for target in family.variants() {
        crate::flashing::Format::from_optional(target.default_binary_format.as_deref())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::flashing::FlashAlgorithm;

    use super::*;
    use std::fs::File;
    type TestResult = Result<(), RegistryError>;

    // Need to synchronize this with probe-rs/tests/scan_chain_test.yaml
    const FIRST_IR_LENGTH: u8 = 4;
    const SECOND_IR_LENGTH: u8 = 6;

    #[cfg(feature = "builtin-targets")]
    #[test]
    fn try_fetch_not_unique() {
        let registry = Registry::from_builtin_families();
        // ambiguous: partially matches STM32G081KBUx and STM32G081KBUxN
        assert!(matches!(
            registry.get_target_by_name("STM32G081KBU"),
            Err(RegistryError::ChipNotUnique(_, _))
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

    #[cfg(feature = "builtin-targets")]
    #[test]
    fn try_fetch2() {
        let registry = Registry::from_builtin_families();
        // ok: matches both STM32G081KBUx and STM32G081KBUxN, but the first one is an exact match
        assert!(registry.get_target_by_name("stm32G081KBUx").is_ok());
    }

    #[cfg(feature = "builtin-targets")]
    #[test]
    fn try_fetch3() {
        let registry = Registry::from_builtin_families();
        // ok: unique substring match
        assert!(registry.get_target_by_name("STM32G081RBI").is_ok());
    }

    #[cfg(feature = "builtin-targets")]
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
            .families
            .iter()
            .flat_map(|family| {
                // Validate all chip descriptors.
                validate_family(family).unwrap();

                // Make additional checks by creating a target for each chip.
                family
                    .variants()
                    .iter()
                    .map(|chip| registry.get_target(family, chip))
            })
            .for_each(|target| {
                // Walk through the flash algorithms and cores and try to create each one.
                for raw_flash_algo in target.flash_algorithms.iter() {
                    for core in raw_flash_algo.cores.iter() {
                        FlashAlgorithm::assemble_from_raw_with_core(raw_flash_algo, core, &target)
                            .unwrap_or_else(|error| {
                                panic!(
                                    "Failed to initialize flash algorithm ({}, {}, {core}): {}",
                                    &target.name, &raw_flash_algo.name, error
                                )
                            });
                    }
                }
            });
    }

    #[test]
    fn add_targets_with_and_without_scanchain() -> TestResult {
        let file = File::open("tests/scan_chain_test.yaml")?;
        add_target_from_yaml(file)?;

        // Check that the scan chain can read from a target correctly
        let mut target = get_target_by_name("FULL_SCAN_CHAIN").unwrap();
        let scan_chain = target.jtag.unwrap().scan_chain.unwrap();
        for device in scan_chain {
            if device.name == Some("core0".to_string()) {
                assert_eq!(device.ir_len, Some(FIRST_IR_LENGTH));
            } else if device.name == Some("ICEPICK".to_string()) {
                assert_eq!(device.ir_len, Some(SECOND_IR_LENGTH));
            }
        }

        // Now check that a device without a scan chain is read correctly
        target = get_target_by_name("NO_JTAG_INFO").unwrap();
        assert_eq!(target.jtag, None);

        // Now check that a device without a scan chain is read correctly
        target = get_target_by_name("NO_SCAN_CHAIN").unwrap();
        assert_eq!(target.jtag.unwrap().scan_chain, None);

        // Check a device with a minimal scan chain
        target = get_target_by_name("PARTIAL_SCAN_CHAIN").unwrap();
        let scan_chain = target.jtag.unwrap().scan_chain.unwrap();
        assert_eq!(scan_chain[0].ir_len, Some(FIRST_IR_LENGTH));
        assert_eq!(scan_chain[1].ir_len, Some(SECOND_IR_LENGTH));

        Ok(())
    }
}
