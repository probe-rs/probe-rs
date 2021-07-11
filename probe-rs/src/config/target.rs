use super::{Chip, Core, CoreType, MemoryRegion, RawFlashAlgorithm, TargetDescriptionSource};

use crate::architecture::arm::sequences::nxp::LPC55S69;
use crate::architecture::arm::sequences::ArmDebugSequence;
use crate::{core::Architecture, flashing::FlashLoader};
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

/// An error occured while parsing the target description.
pub type TargetParseError = serde_yaml::Error;

trait CoreArchitecture {
    fn architecture(&self) -> Architecture;
}

impl CoreArchitecture for CoreType {
    fn architecture(&self) -> Architecture {
        match self {
            CoreType::Riscv => Architecture::Riscv,
            _ => Architecture::Arm,
        }
    }
}

impl Target {
    /// Create a new target
    pub fn new(
        chip: &Chip,
        cores: Vec<Core>,
        flash_algorithms: Vec<RawFlashAlgorithm>,
        source: TargetDescriptionSource,
    ) -> Target {
        // TODO: Figure out how to handle this if cores can have different architectures.
        let mut debug_sequence = match cores[0].core_type.architecture() {
            Architecture::Arm => DebugSequence::Arm(DefaultArmSequence::new()),
            Architecture::Riscv => DebugSequence::Riscv,
        };

        if chip.name.starts_with("LPC55S69") {
            log::warn!("Using custom sequence for LPC55S69");
            debug_sequence = DebugSequence::Arm(LPC55S69::new());
        }

        Target {
            name: chip.name.clone(),
            cores,
            flash_algorithms,
            source,
            memory_map: chip.memory_map.clone(),
            debug_sequence,
        }
    }

    /// Get the architecture of the target
    pub fn architecture(&self) -> Architecture {
        let target_arch = self.cores[0].core_type.architecture();

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
    pub(crate) fn get_memory_region_by_address(&self, address: u32) -> Option<&MemoryRegion> {
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
    Riscv,
}
