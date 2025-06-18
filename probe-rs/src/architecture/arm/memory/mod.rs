//! Types and functions for interacting with target memory.

mod adi_memory_interface;
pub mod romtable;

pub(crate) use adi_memory_interface::ADIMemoryInterface;

use crate::{CoreStatus, memory::MemoryInterface, probe::DebugProbeError};

use super::{ArmDebugInterface, ArmError, FullyQualifiedApAddress};
pub use romtable::{Component, ComponentId, CoresightComponent, PeripheralType, RomTable};

/// Trait for accessing memory behind a memory access port,
pub trait ArmMemoryInterface: MemoryInterface<ArmError> + Send {
    /// The underlying MemoryAp address.
    fn fully_qualified_address(&self) -> FullyQualifiedApAddress;

    /// The underlying memory AP’s base address.
    fn base_address(&mut self) -> Result<u64, ArmError>;

    /// Get this interface as a [`ArmDebugInterface`] object.
    fn get_arm_debug_interface(&mut self) -> Result<&mut dyn ArmDebugInterface, DebugProbeError>;

    /// Get the current value of the CSW reflected in this probe.
    fn generic_status(&mut self) -> Result<crate::architecture::arm::ap::CSW, ArmError>;

    /// Inform the probe of the [`CoreStatus`] of the chip/core attached to
    /// the probe.
    //
    // NOTE: this function should be infallible as it is usually only
    // a visual indication.
    fn update_core_status(&mut self, _state: CoreStatus) {}
}
