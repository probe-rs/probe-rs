use probe_rs_target::{Architecture, ChipFamily};

use super::{Core, MemoryRegion, RawFlashAlgorithm, RegistryError, TargetDescriptionSource};
use crate::architecture::arm::sequences::{
    atsame5x::AtSAME5x,
    infineon::XMC4000,
    nrf52::Nrf52,
    nrf53::Nrf5340,
    nrf91::Nrf9160,
    nxp::{MIMXRT10xx, MIMXRT11xx, LPC55S69},
    stm32f_series::Stm32fSeries,
    stm32h7::Stm32h7,
    ArmDebugSequence,
};
use crate::architecture::riscv::sequences::esp32c3::ESP32C3;
use crate::architecture::riscv::sequences::{DefaultRiscvSequence, RiscvDebugSequence};
use crate::flashing::FlashLoader;
use std::sync::Arc;

use crate::architecture::arm::sequences::DefaultArmSequence;

/// This describes a complete target with a fixed chip model and variant.
#[derive(Clone)]
pub struct Target {
    /// The name of the target.
    pub name: String,
    /// The cores of the target.
    pub cores: Vec<Core>,
    /// The name of the flash algorithm.
    pub flash_algorithms: Vec<RawFlashAlgorithm>,
    /// The memory map of the target.
    pub memory_map: Vec<MemoryRegion>,
    /// Source of the target description. Used for diagnostics.
    pub(crate) source: TargetDescriptionSource,
    /// Debug sequences for the given target.
    pub debug_sequence: DebugSequence,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Target {{
            identifier: {:?},
            flash_algorithms: {:?},
            memory_map: {:?},
        }}",
            self.name, self.flash_algorithms, self.memory_map
        )
    }
}

/// An error occurred while parsing the target description.
pub type TargetParseError = serde_yaml::Error;

impl Target {
    /// Create a new target for the given details.
    ///
    /// We suggest never using this function directly.
    /// Use (crate::registry::Registry::get_target)[`Registry::get_target`] instead.
    /// This will ensure that the used target is valid.
    ///
    /// The user has to make sure that all the cores have the same [`Architecture`].
    /// In any case, this function will always just use the architecture of the first core in any further functionality.
    /// In practice we have never encountered a [`Chip`] with mixed architectures so this should not be of issue.
    ///
    /// Furthermore, the user has to ensure that any [`Core`] in `flash_algorithms[n].cores` is present in `cores` as well.
    pub(crate) fn new(
        family: &ChipFamily,
        chip_name: impl AsRef<str>,
    ) -> Result<Target, RegistryError> {
        // Make sure we are given a valid family:
        family
            .validate()
            .map_err(|e| RegistryError::InvalidChipFamilyDefinition(family.clone(), e))?;

        let chip = family
            .variants
            .iter()
            .find(|chip| chip.name == chip_name.as_ref())
            .ok_or_else(|| RegistryError::ChipNotFound(chip_name.as_ref().to_string()))?;

        let mut flash_algorithms = Vec::new();
        for algo_name in chip.flash_algorithms.iter() {
            let algo = family.get_algorithm(algo_name).expect(
                "The required flash algorithm was not found. This is a bug. Please report it.",
            );

            flash_algorithms.push(algo.clone());
        }

        // We always just take the architecture of the first core which is okay if there is no mixed architectures.
        let mut debug_sequence = match chip.cores[0].core_type.architecture() {
            Architecture::Arm => DebugSequence::Arm(DefaultArmSequence::create()),
            Architecture::Riscv => DebugSequence::Riscv(DefaultRiscvSequence::create()),
        };

        if chip.name.starts_with("MIMXRT10") {
            tracing::warn!("Using custom sequence for MIMXRT10xx");
            debug_sequence = DebugSequence::Arm(MIMXRT10xx::create());
        } else if chip.name.starts_with("MIMXRT11") {
            tracing::warn!("Using custom sequence for MIMXRT11xx");
            debug_sequence = DebugSequence::Arm(MIMXRT11xx::create());
        } else if chip.name.starts_with("LPC55S16") || chip.name.starts_with("LPC55S69") {
            tracing::warn!("Using custom sequence for LPC55S16/LPC55S69");
            debug_sequence = DebugSequence::Arm(LPC55S69::create());
        } else if chip.name.starts_with("esp32c3") {
            tracing::warn!("Using custom sequence for ESP32c3");
            debug_sequence = DebugSequence::Riscv(ESP32C3::create());
        } else if chip.name.starts_with("nRF5340") {
            tracing::warn!("Using custom sequence for nRF5340");
            debug_sequence = DebugSequence::Arm(Nrf5340::create());
        } else if chip.name.starts_with("nRF52") {
            tracing::warn!("Using custom sequence for nRF52");
            debug_sequence = DebugSequence::Arm(Nrf52::create());
        } else if chip.name.starts_with("nRF9160") {
            tracing::warn!("Using custom sequence for nRF9160");
            debug_sequence = DebugSequence::Arm(Nrf9160::create());
        } else if chip.name.starts_with("STM32H7") {
            tracing::warn!("Using custom sequence for STM32H7");
            debug_sequence = DebugSequence::Arm(Stm32h7::create());
        } else if chip.name.starts_with("STM32F1")
            || chip.name.starts_with("STM32F2")
            || chip.name.starts_with("STM32F4")
            || chip.name.starts_with("STM32F7")
        {
            tracing::warn!("Using custom sequence for STM32F1/2/4/7");
            debug_sequence = DebugSequence::Arm(Stm32fSeries::create());
        } else if chip.name.starts_with("ATSAMD5") || chip.name.starts_with("ATSAME5") {
            tracing::warn!("Using custom sequence for {}", chip.name);
            debug_sequence = DebugSequence::Arm(AtSAME5x::create());
        } else if chip.name.starts_with("XMC4") {
            tracing::warn!("Using custom sequence for XMC4000");
            debug_sequence = DebugSequence::Arm(XMC4000::create());
        }

        Ok(Target {
            name: chip.name.clone(),
            cores: chip.cores.clone(),
            flash_algorithms,
            source: family.source.clone(),
            memory_map: chip.memory_map.clone(),
            debug_sequence,
        })
    }

    /// Get the architecture of the target
    pub fn architecture(&self) -> Architecture {
        let target_arch = self.cores[0].core_type.architecture();

        // This should be ensured when a `ChipFamily` is loaded.
        assert!(
            self.cores
                .iter()
                .map(|core| core.core_type.architecture())
                .all(|core_arch| core_arch == target_arch),
            "Not all cores of the target are of the same architecture. Probe-rs doesn't support this (yet). If you see this, it is a bug. Please file an issue."
        );

        target_arch
    }

    /// Source description of this target.
    pub fn source(&self) -> &TargetDescriptionSource {
        &self.source
    }

    /// Create a [FlashLoader] for this target, which can be used
    /// to program its non-volatile memory.
    pub fn flash_loader(&self) -> FlashLoader {
        FlashLoader::new(self.memory_map.clone(), self.source.clone())
    }

    /// Gets a [RawFlashAlgorithm] by name.
    pub(crate) fn flash_algorithm_by_name(&self, name: &str) -> Option<&RawFlashAlgorithm> {
        self.flash_algorithms.iter().find(|a| a.name == name)
    }

    /// Gets the core index from the core name
    pub(crate) fn core_index_by_name(&self, name: &str) -> Option<usize> {
        self.cores.iter().position(|c| c.name == name)
    }

    /// Gets the first found [MemoryRegion] that contains the given address
    pub(crate) fn get_memory_region_by_address(&self, address: u64) -> Option<&MemoryRegion> {
        self.memory_map.iter().find(|region| match region {
            MemoryRegion::Ram(rr) if rr.range.contains(&address) => true,
            MemoryRegion::Generic(gr) if gr.range.contains(&address) => true,
            MemoryRegion::Nvm(nr) if nr.range.contains(&address) => true,
            _ => false,
        })
    }
}

/// Selector for the debug target.
#[derive(Debug, Clone)]
pub enum TargetSelector {
    /// Specify the name of a target, which will
    /// be used to search the internal list of
    /// targets.
    Unspecified(String),
    /// Directly specify a target.
    Specified(Target),
    /// Try to automatically identify the target,
    /// by reading identifying information from
    /// the probe and / or target.
    Auto,
}

impl From<&str> for TargetSelector {
    fn from(value: &str) -> Self {
        TargetSelector::Unspecified(value.into())
    }
}

impl From<&String> for TargetSelector {
    fn from(value: &String) -> Self {
        TargetSelector::Unspecified(value.into())
    }
}

impl From<String> for TargetSelector {
    fn from(value: String) -> Self {
        TargetSelector::Unspecified(value)
    }
}

impl From<()> for TargetSelector {
    fn from(_value: ()) -> Self {
        TargetSelector::Auto
    }
}

impl From<Target> for TargetSelector {
    fn from(target: Target) -> Self {
        TargetSelector::Specified(target)
    }
}

/// This is the type to denote a general debug sequence.  
/// It can differentiate between ARM and RISC-V for now.  
/// Currently, only the ARM variant does something sensible;  
/// RISC-V will be ignored when encountered.
#[derive(Clone)]
pub enum DebugSequence {
    /// An ARM debug sequence.
    Arm(Arc<dyn ArmDebugSequence>),
    /// A RISC-V debug sequence.
    Riscv(Arc<dyn RiscvDebugSequence>),
}
