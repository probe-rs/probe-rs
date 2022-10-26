use crate::CoreAccessOptions;

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
}

impl CoreType {
    /// Returns true if the core type is an ARM Cortex-M
    pub fn is_cortex_m(&self) -> bool {
        matches!(
            self,
            CoreType::Armv6m | CoreType::Armv7em | CoreType::Armv7m | CoreType::Armv8m
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
}

impl CoreType {
    /// Returns the parent architecture family of this core type.
    pub fn architecture(&self) -> Architecture {
        match self {
            CoreType::Riscv => Architecture::Riscv,
            _ => Architecture::Arm,
        }
    }
}

/// Instruction set used by a core
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
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
}

impl InstructionSet {
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
        }
    }
    /// Get the maximum instruction size in bytes. All supported architectures have a maximum instruction size of 4 bytes.
    pub fn get_maximum_instruction_size(&self) -> u8 {
        4
    }
}

/// This describes a chip family with all its variants.
///
/// This struct is usually read from a target description
/// file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipFamily {
    /// This is the name of the chip family in base form.
    /// E.g. `nRF52832`.
    pub name: String,
    /// The JEP106 code of the manufacturer.
    pub manufacturer: Option<JEP106Code>,
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
        // We check each variant if it is valid.
        // If one is not valid, we abort with an appropriate error message.
        for variant in &self.variants {
            // Make sure the algorithms used on the variant actually exist on the family (this is basically a check for typos).
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

            // Check that there is at least one core.
            if let Some(core) = variant.cores.get(0) {
                // Make sure that the core types (architectures) are not mixed.
                let architecture = core.core_type.architecture();
                if variant
                    .cores
                    .iter()
                    .any(|core| core.core_type.architecture() != architecture)
                {
                    return Err(format!(
                        "definition for variant `{}` contains mixed core architectures",
                        variant.name
                    ));
                }
            } else {
                return Err(format!(
                    "definition for variant `{}` does not contain any cores",
                    variant.name
                ));
            }

            // Core specific validation logic based on type
            for core in variant.cores.iter() {
                // The core access options must match the core type specified
                match &core.core_access_options {
                    CoreAccessOptions::Arm(options) => {
                        if !matches!(
                            core.core_type,
                            CoreType::Armv6m
                                | CoreType::Armv7a
                                | CoreType::Armv7em
                                | CoreType::Armv7m
                                | CoreType::Armv8a
                                | CoreType::Armv8m
                        ) {
                            return Err(format!(
                                "Arm options don't match core type {:?} on core {}",
                                core.core_type, core.name
                            ));
                        }

                        if matches!(core.core_type, CoreType::Armv7a | CoreType::Armv8a)
                            && options.debug_base.is_none()
                        {
                            return Err(format!("Core {} requires setting debug_base", core.name));
                        }

                        if core.core_type == CoreType::Armv8a && options.cti_base.is_none() {
                            return Err(format!("Core {} requires setting cti_base", core.name));
                        }
                    }
                    CoreAccessOptions::Riscv(_) => {
                        if core.core_type != CoreType::Riscv {
                            return Err(format!(
                                "Riscv options don't match core type {:?} on core {}",
                                core.core_type, core.name
                            ));
                        }
                    }
                }
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
}
