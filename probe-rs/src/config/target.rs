use super::{
    sequences::{
        atsam::AtSAM,
        efm32xg2::EFM32xG2,
        esp32::ESP32,
        esp32c2::ESP32C2,
        esp32c3::ESP32C3,
        esp32c6::ESP32C6,
        esp32h2::ESP32H2,
        esp32s2::ESP32S2,
        esp32s3::ESP32S3,
        infineon::XMC4000,
        nrf52::Nrf52,
        nrf53::Nrf5340,
        nrf91::Nrf9160,
        nxp_armv7m::{MIMXRT10xx, MIMXRT11xx},
        nxp_armv8m::{LPC55Sxx, MIMXRT5xxS},
        stm32_armv6::{Stm32Armv6, Stm32Armv6Family},
        stm32_armv7::Stm32Armv7,
        stm32h7::Stm32h7,
    },
    Core, MemoryRegion, RawFlashAlgorithm, RegistryError, TargetDescriptionSource,
};
use crate::architecture::{
    arm::{
        ap::MemoryAp,
        sequences::{ArmDebugSequence, DefaultArmSequence},
        ApAddress, DpAddress,
    },
    riscv::sequences::{DefaultRiscvSequence, RiscvDebugSequence},
    xtensa::sequences::{DefaultXtensaSequence, XtensaDebugSequence},
};
use crate::flashing::FlashLoader;
use probe_rs_target::{Architecture, BinaryFormat, ChipFamily, Jtag, MemoryRange};
use std::sync::Arc;

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
    /// The regions of memory to scan to try to find an RTT header.
    ///
    /// Each region must be enclosed in exactly one RAM region from
    /// `memory_map`.
    pub rtt_scan_regions: Vec<std::ops::Range<u64>>,
    /// The Description of the scan chain
    ///
    /// The scan chain can be parsed from the CMSIS-SDF file, or specified
    /// manually in the target.yaml file. It is used by some probes to determine
    /// the number devices in the scan chain and their ir lengths.
    pub jtag: Option<Jtag>,
    /// The default executable format for the target.
    pub default_format: BinaryFormat,
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
            .map_err(|e| RegistryError::InvalidChipFamilyDefinition(Box::new(family.clone()), e))?;

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

        let debug_sequence = if chip.name.starts_with("MIMXRT10") {
            DebugSequence::Arm(MIMXRT10xx::create())
        } else if chip.name.starts_with("MIMXRT11") {
            DebugSequence::Arm(MIMXRT11xx::create())
        } else if chip.name.starts_with("MIMXRT5") {
            DebugSequence::Arm(MIMXRT5xxS::create())
        } else if chip.name.starts_with("LPC55S16")
            || chip.name.starts_with("LPC55S26")
            || chip.name.starts_with("LPC55S28")
            || chip.name.starts_with("LPC55S66")
            || chip.name.starts_with("LPC55S69")
        {
            DebugSequence::Arm(LPC55Sxx::create())
        } else if chip.name.starts_with("EFM32PG2")
            || chip.name.starts_with("EFR32BG2")
            || chip.name.starts_with("EFR32FG2")
            || chip.name.starts_with("EFR32MG2")
            || chip.name.starts_with("EFR32ZG2")
        {
            DebugSequence::Arm(EFM32xG2::create())
        } else if chip.name.starts_with("esp32-") {
            DebugSequence::Xtensa(ESP32::create(chip))
        } else if chip.name.eq_ignore_ascii_case("esp32s2") {
            DebugSequence::Xtensa(ESP32S2::create(chip))
        } else if chip.name.eq_ignore_ascii_case("esp32s3") {
            DebugSequence::Xtensa(ESP32S3::create(chip))
        } else if chip.name.eq_ignore_ascii_case("esp32c2") {
            DebugSequence::Riscv(ESP32C2::create(chip))
        } else if chip.name.eq_ignore_ascii_case("esp32c3") {
            DebugSequence::Riscv(ESP32C3::create(chip))
        } else if chip.name.eq_ignore_ascii_case("esp32c6") {
            DebugSequence::Riscv(ESP32C6::create(chip))
        } else if chip.name.eq_ignore_ascii_case("esp32h2") {
            DebugSequence::Riscv(ESP32H2::create(chip))
        } else if chip.name.starts_with("nRF5340") {
            DebugSequence::Arm(Nrf5340::create())
        } else if chip.name.starts_with("nRF52") {
            DebugSequence::Arm(Nrf52::create())
        } else if chip.name.starts_with("nRF9160") {
            DebugSequence::Arm(Nrf9160::create())
        } else if chip.name.starts_with("STM32F0") {
            DebugSequence::Arm(Stm32Armv6::create(Stm32Armv6Family::F0))
        } else if chip.name.starts_with("STM32L0") {
            DebugSequence::Arm(Stm32Armv6::create(Stm32Armv6Family::L0))
        } else if chip.name.starts_with("STM32G0") {
            DebugSequence::Arm(Stm32Armv6::create(Stm32Armv6Family::G0))
        } else if chip.name.starts_with("STM32F1")
            || chip.name.starts_with("STM32F2")
            || chip.name.starts_with("STM32F3")
            || chip.name.starts_with("STM32F4")
            || chip.name.starts_with("STM32F7")
            || chip.name.starts_with("STM32G4")
            || chip.name.starts_with("STM32L1")
            || chip.name.starts_with("STM32L4")
            || chip.name.starts_with("STM32WB")
            || chip.name.starts_with("STM32WL")
        {
            DebugSequence::Arm(Stm32Armv7::create())
        } else if chip.name.starts_with("STM32H7") {
            DebugSequence::Arm(Stm32h7::create())
        } else if chip.name.starts_with("ATSAMD1")
            || chip.name.starts_with("ATSAMD2")
            || chip.name.starts_with("ATSAMDA")
            || chip.name.starts_with("ATSAMD5")
            || chip.name.starts_with("ATSAME5")
        {
            DebugSequence::Arm(AtSAM::create())
        } else if chip.name.starts_with("XMC4") {
            DebugSequence::Arm(XMC4000::create())
        } else {
            // Default to the architecture of the first core, which is okay if
            // there is no mixed architectures.
            match chip.cores[0].core_type.architecture() {
                Architecture::Arm => DebugSequence::Arm(DefaultArmSequence::create()),
                Architecture::Riscv => DebugSequence::Riscv(DefaultRiscvSequence::create()),
                Architecture::Xtensa => DebugSequence::Xtensa(DefaultXtensaSequence::create()),
            }
        };

        tracing::info!("Using sequence {:?}", debug_sequence);

        let ram_regions = chip.memory_map.iter().filter_map(MemoryRegion::ram_region);
        let rtt_scan_regions = match &chip.rtt_scan_ranges {
            Some(ranges) => {
                // The custom ranges must all be enclosed by exactly one of
                // the defined RAM regions.
                for rng in ranges {
                    if !ram_regions
                        .clone()
                        .any(|region| region.range.contains_range(rng))
                    {
                        return Err(RegistryError::InvalidRttScanRange(rng.clone()));
                    }
                }
                ranges.clone()
            }
            None => {
                // By default we use all of the RAM ranges from the memory map.
                ram_regions.map(|region| region.range.clone()).collect()
            }
        };

        Ok(Target {
            name: chip.name.clone(),
            cores: chip.cores.clone(),
            flash_algorithms,
            source: family.source.clone(),
            memory_map: chip.memory_map.clone(),
            debug_sequence,
            rtt_scan_regions,
            jtag: chip.jtag.clone(),
            default_format: chip.default_binary_format.clone().unwrap_or_default(),
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

    /// Return the default core of the target, usually the first core.
    ///
    /// This core should be used for operations such as debug_unlock,
    /// when nothing else is specified.
    pub fn default_core(&self) -> &Core {
        // TODO: Check if this is specified in the target description.
        &self.cores[0]
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

    /// Returns a [RawFlashAlgorithm] by name.
    pub(crate) fn flash_algorithm_by_name(&self, name: &str) -> Option<&RawFlashAlgorithm> {
        self.flash_algorithms.iter().find(|a| a.name == name)
    }

    /// Gets the core index from the core name
    pub(crate) fn core_index_by_name(&self, name: &str) -> Option<usize> {
        self.cores.iter().position(|c| c.name == name)
    }

    /// Gets the first found [MemoryRegion] that contains the given address
    pub(crate) fn get_memory_region_by_address(&self, address: u64) -> Option<&MemoryRegion> {
        self.memory_map
            .iter()
            .find(|region| region.contains(address))
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

impl From<Option<&str>> for TargetSelector {
    fn from(value: Option<&str>) -> Self {
        match value {
            Some(identifier) => identifier.into(),
            None => TargetSelector::Auto,
        }
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
#[derive(Clone, Debug)]
pub enum DebugSequence {
    /// An ARM debug sequence.
    Arm(Arc<dyn ArmDebugSequence>),
    /// A RISC-V debug sequence.
    Riscv(Arc<dyn RiscvDebugSequence>),
    /// An Xtensa debug sequence.
    Xtensa(Arc<dyn XtensaDebugSequence>),
}

pub(crate) trait CoreExt {
    // Retrieve the Coresight MemoryAP which should be used to
    // access the core, if available.
    fn memory_ap(&self) -> Option<MemoryAp>;
}

impl CoreExt for Core {
    fn memory_ap(&self) -> Option<MemoryAp> {
        match &self.core_access_options {
            probe_rs_target::CoreAccessOptions::Arm(options) => Some(MemoryAp::new(ApAddress {
                dp: match options.psel {
                    0 => DpAddress::Default,
                    x => DpAddress::Multidrop(x),
                },
                ap: options.ap,
            })),
            probe_rs_target::CoreAccessOptions::Riscv(_) => None,
            probe_rs_target::CoreAccessOptions::Xtensa(_) => None,
        }
    }
}
