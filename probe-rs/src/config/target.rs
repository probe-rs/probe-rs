use super::{Chip, Core, CoreType, MemoryRegion, RawFlashAlgorithm, TargetDescriptionSource};
use crate::{core::Architecture, flashing::FlashLoader};
use std::sync::Arc;

use crate::{architecture::arm::ArmCommunicationInterface, DebugProbeError, Error, Memory};

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
    pub debug_sequence: Arc<DebugSequence>,
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

impl Target {
    /// Create a new target
    pub fn new(
        chip: &Chip,
        cores: Vec<Core>,
        flash_algorithms: Vec<RawFlashAlgorithm>,
        source: TargetDescriptionSource,
    ) -> Target {
        Target {
            name: chip.name.clone(),
            cores,
            flash_algorithms,
            memory_map: chip.memory_map.clone(),
            source,
            debug_sequence: Arc::new(DebugSequence::Riscv),
        }
    }

    /// Get the architecture of the target
    pub fn architecture(&self) -> Architecture {
        fn get_architecture_from_core(core_type: CoreType) -> Architecture {
            match core_type {
                CoreType::M0 => Architecture::Arm,
                CoreType::M3 => Architecture::Arm,
                CoreType::M33 => Architecture::Arm,
                CoreType::M4 => Architecture::Arm,
                CoreType::M7 => Architecture::Arm,
                CoreType::Riscv => Architecture::Riscv,
            }
        }

        let target_arch = get_architecture_from_core(self.cores[0].core_type);

        assert!(
            self.cores
                .iter()
                .map(|core| get_architecture_from_core(core.core_type))
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

pub enum DebugSequence {
    Arm(Box<dyn ArmDebugSequence>),
    Riscv,
}

pub trait ArmDebugSequence: Send + Sync {
    fn reset_hardware_assert(&self, interface: &mut Memory) -> Result<(), Error>;
    fn reset_hardware_deassert(&self, interface: &mut Memory) -> Result<(), Error>;

    fn debug_port_setup(&self, interface: &mut Memory) -> Result<(), Error>;

    fn debug_port_start(&self, interface: &mut Memory) -> Result<(), Error>;

    fn debug_device_unlock(&self, interface: &mut Memory) -> Result<(), Error> {
        // Empty by default
        Ok(())
    }

    fn debug_core_start(&self, interface: &mut Memory) -> Result<(), Error>;

    fn recover_support_start(&self, _interface: &mut Memory) -> Result<(), Error> {
        // Empty by default
        Ok(())
    }

    fn reset_catch_set(&self, interface: &mut Memory) -> Result<(), Error>;

    fn reset_catch_clear(&self, interface: &mut Memory) -> Result<(), Error>;

    fn reset_system(&self, interface: &mut Memory) -> Result<(), Error>;
}
