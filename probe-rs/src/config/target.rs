use super::{Core, MemoryRegion, RawFlashAlgorithm, TargetDescriptionSource};
use crate::flashing::FlashLoader;
use crate::{
    architecture::{
        arm::{
            sequences::{ArmDebugSequence, DefaultArmSequence},
            DpAddress, FullyQualifiedApAddress,
        },
        riscv::sequences::{DefaultRiscvSequence, RiscvDebugSequence},
        xtensa::sequences::{DefaultXtensaSequence, XtensaDebugSequence},
    },
    rtt::ScanRegion,
};
use probe_rs_target::{
    Architecture, Chip, ChipFamily, Jtag, MemoryAccess, MemoryRange as _, NvmRegion,
};
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
    pub rtt_scan_regions: ScanRegion,
    /// The Description of the scan chain
    ///
    /// The scan chain can be parsed from the CMSIS-SDF file, or specified
    /// manually in the target.yaml file. It is used by some probes to determine
    /// the number devices in the scan chain and their ir lengths.
    pub jtag: Option<Jtag>,
    /// The default executable format for the target.
    pub default_format: Option<String>,
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

impl Target {
    /// Create a new target for the given details.
    ///
    /// The given chip must be a member of the given family.
    pub(super) fn new(family: &ChipFamily, chip: &Chip) -> Target {
        let mut memory_map = chip.memory_map.clone();
        let mut flash_algorithms = Vec::new();
        for algo_name in chip.flash_algorithms.iter() {
            let algo = family
                .get_algorithm(algo_name)
                .expect(
                    "The required flash algorithm was not found. This is a bug. Please report it.",
                )
                .clone();

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

            // If the flash algorithm addresses a range that is not covered by any memory region,
            // we add a new memory region for it. This is usually the case for option bytes and
            // some OTP memory regions in certain CMSIS packs.
            let algo_range = &algo.flash_properties.address_range;
            if !memory_map
                .iter()
                .any(|region| region.address_range().intersects_range(algo_range))
            {
                // HACK (doubly, even):
                // Conjure up a memory region for the flash algorithm. This is mostly used by
                // EEPROM regions. We probably don't want to erase these regions, so we set
                // `is_alias`, which then causes "erase all" to skip the region.
                memory_map.push(MemoryRegion::Nvm(NvmRegion {
                    name: None,
                    range: algo_range.clone(),
                    cores: algo_cores,
                    is_alias: true,
                    access: Some(MemoryAccess {
                        read: false,
                        write: false,
                        execute: false,
                        boot: false,
                    }),
                }));
            }

            flash_algorithms.push(algo);
        }

        let debug_sequence = crate::vendor::try_create_debug_sequence(chip).unwrap_or_else(|| {
            // Default to the architecture of the first core, which is okay if
            // there is no mixed architectures.
            match chip.cores[0].core_type.architecture() {
                Architecture::Arm => DebugSequence::Arm(DefaultArmSequence::create()),
                Architecture::Riscv => DebugSequence::Riscv(DefaultRiscvSequence::create()),
                Architecture::Xtensa => DebugSequence::Xtensa(DefaultXtensaSequence::create()),
            }
        });

        tracing::info!("Using sequence {:?}", debug_sequence);

        let rtt_scan_regions = match &chip.rtt_scan_ranges {
            Some(ranges) => ScanRegion::Ranges(ranges.clone()),
            None => ScanRegion::Ram, // By default we use all of the RAM ranges from the memory map.
        };

        Target {
            name: chip.name.clone(),
            cores: chip.cores.clone(),
            flash_algorithms,
            source: family.source.clone(),
            memory_map,
            debug_sequence,
            rtt_scan_regions,
            jtag: chip.jtag.clone(),
            default_format: chip.default_binary_format.clone(),
        }
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

    /// Returns the core index from the core name
    pub fn core_index_by_name(&self, name: &str) -> Option<usize> {
        self.cores.iter().position(|c| c.name == name)
    }

    /// Returns the core index from the core name
    pub fn core_index_by_address(&self, address: u64) -> Option<usize> {
        let target_memory = self.memory_region_by_address(address)?;
        let core_name = target_memory.cores().first()?;
        self.core_index_by_name(core_name)
    }

    /// Returns the first found [MemoryRegion] that contains the given address
    pub fn memory_region_by_address(&self, address: u64) -> Option<&MemoryRegion> {
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
    fn memory_ap(&self) -> Option<FullyQualifiedApAddress>;
}

impl CoreExt for Core {
    fn memory_ap(&self) -> Option<FullyQualifiedApAddress> {
        match &self.core_access_options {
            probe_rs_target::CoreAccessOptions::Arm(options) => {
                Some(FullyQualifiedApAddress::v1_with_dp(
                    match options.psel {
                        0 => DpAddress::Default,
                        x => DpAddress::Multidrop(x),
                    },
                    options.ap,
                ))
            }
            probe_rs_target::CoreAccessOptions::Riscv(_) => None,
            probe_rs_target::CoreAccessOptions::Xtensa(_) => None,
        }
    }
}
