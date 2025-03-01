use crate::memory::RegionMergeIterator as _;
use crate::serialize::hex_jep106_option;
use crate::{CoreAccessOptions, chip_detection::ChipDetectionMethod};
use crate::{MemoryRange, MemoryRegion};

use super::chip::Chip;
use super::flash_algorithm::RawFlashAlgorithm;
use jep106::JEP106Code;

use serde::{Deserialize, Serialize};

/// Source of a target description.
///
/// This is used for diagnostics, when
/// an error related to a target description occurs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetDescriptionSource {
    /// The target description is a generic target description,
    /// which just describes a core type (e.g. M4), without any
    /// flash algorithm or memory description.
    Generic,
    /// The target description is a built-in target description,
    /// which was included into probe-rs at compile time.
    BuiltIn,
    /// The target description was from an external source
    /// during runtime.
    External,
}

/// Type of a supported core.
#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreType {
    /// ARMv6-M: Cortex M0, M0+, M1
    Armv6m,
    /// ARMv7-A: Cortex A7, A9, A15
    Armv7a,
    /// ARMv7-M: Cortex M3
    Armv7m,
    /// ARMv7e-M: Cortex M4, M7
    Armv7em,
    /// ARMv7-A: Cortex A35, A55, A72
    Armv8a,
    /// ARMv8-M: Cortex M23, M33
    Armv8m,
    /// RISC-V
    Riscv,
    /// Xtensa - TODO: may need to split into NX, LX6 and LX7
    Xtensa,
}

impl CoreType {
    /// Returns true if the core type is an ARM Cortex-M
    pub fn is_cortex_m(&self) -> bool {
        matches!(
            self,
            CoreType::Armv6m | CoreType::Armv7em | CoreType::Armv7m | CoreType::Armv8m
        )
    }

    fn is_riscv(&self) -> bool {
        matches!(self, CoreType::Riscv)
    }

    fn is_xtensa(&self) -> bool {
        matches!(self, CoreType::Xtensa)
    }

    fn is_arm(&self) -> bool {
        matches!(
            self,
            CoreType::Armv6m
                | CoreType::Armv7a
                | CoreType::Armv7em
                | CoreType::Armv7m
                | CoreType::Armv8a
                | CoreType::Armv8m
        )
    }
}

/// The architecture family of a specific [`CoreType`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Architecture {
    /// An ARM core of one of the specific types [`CoreType::Armv6m`], [`CoreType::Armv7m`], [`CoreType::Armv7em`] or [`CoreType::Armv8m`]
    Arm,
    /// A RISC-V core.
    Riscv,
    /// An Xtensa core.
    Xtensa,
}

impl CoreType {
    /// Returns the parent architecture family of this core type.
    pub fn architecture(&self) -> Architecture {
        match self {
            CoreType::Riscv => Architecture::Riscv,
            CoreType::Xtensa => Architecture::Xtensa,
            _ => Architecture::Arm,
        }
    }
}

/// Instruction set used by a core
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstructionSet {
    /// ARM Thumb 2 instruction set
    Thumb2,
    /// ARM A32 (often just called ARM) instruction set
    A32,
    /// ARM A64 (aarch64) instruction set
    A64,
    /// RISC-V 32-bit uncompressed instruction sets (RV32) - covers all ISA variants that use 32-bit instructions.
    RV32,
    /// RISC-V 32-bit compressed instruction sets (RV32C) - covers all ISA variants that allow compressed 16-bit instructions.
    RV32C,
    /// Xtensa instruction set
    Xtensa,
}

impl InstructionSet {
    /// Get the instruction set from a rustc target triple.
    pub fn from_target_triple(triple: &str) -> Option<Self> {
        match triple.split('-').next()? {
            "thumbv6m" | "thumbv7em" | "thumbv7m" | "thumbv8m" => Some(InstructionSet::Thumb2),
            "arm" => Some(InstructionSet::A32),
            "aarch64" => Some(InstructionSet::A64),
            "xtensa" => Some(InstructionSet::Xtensa),
            other => {
                if let Some(features) = other.strip_prefix("riscv32") {
                    if features.contains('c') {
                        Some(InstructionSet::RV32C)
                    } else {
                        Some(InstructionSet::RV32)
                    }
                } else {
                    None
                }
            }
        }
    }

    /// Get the minimum instruction size in bytes.
    pub fn get_minimum_instruction_size(&self) -> u8 {
        match self {
            InstructionSet::Thumb2 => {
                // Thumb2 uses a variable size (2 or 4) instruction set. For our purposes, we set it as 2, so that we don't accidentally read outside of addressable memory.
                2
            }
            InstructionSet::A32 => 4,
            InstructionSet::A64 => 4,
            InstructionSet::RV32 => 4,
            InstructionSet::RV32C => 2,
            InstructionSet::Xtensa => 2,
        }
    }
    /// Get the maximum instruction size in bytes. All supported architectures have a maximum instruction size of 4 bytes.
    pub fn get_maximum_instruction_size(&self) -> u8 {
        // TODO: Xtensa may have wide instructions
        4
    }

    /// Returns whether a CPU with the `self` instruction set is compatible with a program compiled for `instr_set`.
    pub fn is_compatible(&self, instr_set: InstructionSet) -> bool {
        if *self == instr_set {
            return true;
        }

        matches!(
            (self, instr_set),
            (InstructionSet::RV32C, InstructionSet::RV32)
        )
    }
}

/// This describes a chip family with all its variants.
///
/// This struct is usually read from a target description
/// file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChipFamily {
    /// This is the name of the chip family in base form.
    /// E.g. `nRF52832`.
    pub name: String,
    /// The JEP106 code of the manufacturer.
    #[serde(serialize_with = "hex_jep106_option")]
    pub manufacturer: Option<JEP106Code>,
    /// The method(s) that may be able to identify targets in this family.
    #[serde(default)]
    pub chip_detection: Vec<ChipDetectionMethod>,
    /// The `target-gen` process will set this to `true`.
    /// Please change this to `false` if this file is modified from the generated, or is a manually created target description.
    #[serde(default)]
    pub generated_from_pack: bool,
    /// The latest release of the pack file from which this was generated.
    /// Values:
    /// - `Some("1.3.0")` if the latest pack file release was for example "1.3.0".
    /// - `None` if this was not generated from a pack file, or has been modified since it was generated.
    #[serde(default)]
    pub pack_file_release: Option<String>,
    /// This vector holds all the variants of the family.
    pub variants: Vec<Chip>,
    /// This vector holds all available algorithms.
    #[serde(default)]
    pub flash_algorithms: Vec<RawFlashAlgorithm>,
    #[serde(skip, default = "default_source")]
    /// Source of the target description, used for diagnostics
    pub source: TargetDescriptionSource,
}

fn default_source() -> TargetDescriptionSource {
    TargetDescriptionSource::External
}

impl ChipFamily {
    /// Validates the [`ChipFamily`] such that probe-rs can make assumptions about the correctness without validating thereafter.
    ///
    /// This method should be called right after the [`ChipFamily`] is created!
    pub fn validate(&self) -> Result<(), String> {
        self.reject_duplicate_target_names()?;
        self.ensure_algorithms_exists()?;
        self.ensure_at_least_one_core()?;
        self.reject_incorrect_core_access_options()?;
        self.validate_memory_regions()?;
        self.validate_rtt_scan_regions()?;

        Ok(())
    }

    /// Rejects target descriptions with duplicate target names. Only one of these targets can
    /// be selected, so having multiple is probably a mistake.
    fn reject_duplicate_target_names(&self) -> Result<(), String> {
        use std::collections::HashSet;

        let mut seen = HashSet::new();

        for chip in &self.variants {
            if !seen.insert(&chip.name) {
                return Err(format!(
                    "Target {} appears multiple times in {}",
                    chip.name, self.name,
                ));
            }
        }

        Ok(())
    }

    /// Make sure the algorithms used on the variant actually exist on the family (this is basically a check for typos).
    fn ensure_algorithms_exists(&self) -> Result<(), String> {
        for variant in &self.variants {
            for algorithm_name in variant.flash_algorithms.iter() {
                if !self
                    .flash_algorithms
                    .iter()
                    .any(|algorithm| &algorithm.name == algorithm_name)
                {
                    return Err(format!(
                        "unknown flash algorithm `{}` for variant `{}`",
                        algorithm_name, variant.name
                    ));
                }
            }
        }

        Ok(())
    }

    // Check that there is at least one core.
    fn ensure_at_least_one_core(&self) -> Result<(), String> {
        for variant in &self.variants {
            let Some(core) = variant.cores.first() else {
                return Err(format!(
                    "variant `{}` does not contain any cores",
                    variant.name
                ));
            };

            // Make sure that the core types (architectures) are not mixed.
            let architecture = core.core_type.architecture();
            if variant
                .cores
                .iter()
                .any(|core| core.core_type.architecture() != architecture)
            {
                return Err(format!(
                    "variant `{}` contains mixed core architectures",
                    variant.name
                ));
            }
        }

        Ok(())
    }

    fn reject_incorrect_core_access_options(&self) -> Result<(), String> {
        // We check each variant if it is valid.
        // If one is not valid, we abort with an appropriate error message.
        for variant in &self.variants {
            // Core specific validation logic based on type
            for core in variant.cores.iter() {
                // The core access options must match the core type specified
                match &core.core_access_options {
                    CoreAccessOptions::Arm(_) if !core.core_type.is_arm() => {
                        return Err(format!(
                            "Arm options don't match core type {:?} on core {}",
                            core.core_type, core.name
                        ));
                    }
                    CoreAccessOptions::Riscv(_) if !core.core_type.is_riscv() => {
                        return Err(format!(
                            "Riscv options don't match core type {:?} on core {}",
                            core.core_type, core.name
                        ));
                    }
                    CoreAccessOptions::Xtensa(_) if !core.core_type.is_xtensa() => {
                        return Err(format!(
                            "Xtensa options don't match core type {:?} on core {}",
                            core.core_type, core.name
                        ));
                    }
                    CoreAccessOptions::Arm(options) => {
                        if matches!(core.core_type, CoreType::Armv7a | CoreType::Armv8a)
                            && options.debug_base.is_none()
                        {
                            return Err(format!("Core {} requires setting debug_base", core.name));
                        }

                        if core.core_type == CoreType::Armv8a && options.cti_base.is_none() {
                            return Err(format!("Core {} requires setting cti_base", core.name));
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(())
    }

    /// Ensures that the memory is assigned to a core, and that all the cores exist
    fn validate_memory_regions(&self) -> Result<(), String> {
        for variant in &self.variants {
            let core_names = variant
                .cores
                .iter()
                .map(|core| &core.name)
                .collect::<Vec<_>>();

            for memory in &variant.memory_map {
                for core in memory.cores() {
                    if !core_names.contains(&core) {
                        return Err(format!(
                            "Variant {}, memory region {:?} is assigned to a non-existent core {}",
                            variant.name, memory, core
                        ));
                    }
                }

                if memory.cores().is_empty() {
                    return Err(format!(
                        "Variant {}, memory region {:?} is not assigned to a core",
                        variant.name, memory
                    ));
                }
            }
        }

        Ok(())
    }

    fn validate_rtt_scan_regions(&self) -> Result<(), String> {
        for variant in &self.variants {
            let Some(rtt_scan_ranges) = &variant.rtt_scan_ranges else {
                return Ok(());
            };

            let ram_regions = variant
                .memory_map
                .iter()
                .filter_map(MemoryRegion::as_ram_region)
                .merge_consecutive()
                .collect::<Vec<_>>();

            // The custom ranges must all be enclosed by exactly one of
            // the defined RAM regions.
            for scan_range in rtt_scan_ranges {
                if ram_regions
                    .iter()
                    .any(|region| region.range.contains_range(scan_range))
                {
                    continue;
                }

                return Err(format!(
                    "The RTT scan region ({:#010x?}) of {} is not enclosed by any single RAM region.",
                    scan_range, variant.name,
                ));
            }
        }

        Ok(())
    }
}

impl ChipFamily {
    /// Get the different [Chip]s which are part of this
    /// family.
    pub fn variants(&self) -> &[Chip] {
        &self.variants
    }

    /// Get all flash algorithms for this family of chips.
    pub fn algorithms(&self) -> &[RawFlashAlgorithm] {
        &self.flash_algorithms
    }

    /// Try to find a [RawFlashAlgorithm] with a given name.
    pub fn get_algorithm(&self, name: impl AsRef<str>) -> Option<&RawFlashAlgorithm> {
        let name = name.as_ref();
        self.flash_algorithms.iter().find(|elem| elem.name == name)
    }

    /// Tries to find a [RawFlashAlgorithm] with a given name and returns it with the
    /// core assignment fixed to the cores of the given chip.
    pub fn get_algorithm_for_chip(
        &self,
        name: impl AsRef<str>,
        chip: &Chip,
    ) -> Option<RawFlashAlgorithm> {
        self.get_algorithm(name).map(|algo| {
            let mut algo_cores = if algo.cores.is_empty() {
                chip.cores.iter().map(|core| core.name.clone()).collect()
            } else {
                algo.cores.clone()
            };

            // only keep cores in the algo that are also in the chip
            algo_cores.retain(|algo_core| {
                chip.cores
                    .iter()
                    .any(|chip_core| &chip_core.name == algo_core)
            });

            RawFlashAlgorithm {
                cores: algo_cores,
                ..algo.clone()
            }
        })
    }
}
