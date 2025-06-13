//! Types and functions for interacting with target memory.

mod adi_memory_interface;
pub mod romtable;

pub(crate) use adi_memory_interface::ADIMemoryInterface;

use crate::{CoreStatus, memory::MemoryInterface, probe::DebugProbeError};

use super::{
    ArmError, ArmDebugInterface, DapAccess, FullyQualifiedApAddress,
    communication_interface::SwdSequence,
};
pub use romtable::{Component, ComponentId, CoresightComponent, PeripheralType, RomTable};

/// An ArmMemoryInterface (ArmProbeInterface + MemoryAp)
pub trait ArmMemoryInterface: MemoryInterface<ArmError> {
    /// The underlying MemoryAp address.
    fn fully_qualified_address(&self) -> FullyQualifiedApAddress;

    /// The underlying memory AP’s base address.
    fn base_address(&mut self) -> Result<u64, ArmError>;

    /// Get this interface as a SwdSequence object.
    fn get_swd_sequence(&mut self) -> Result<&mut dyn SwdSequence, DebugProbeError>;

    /// Get this interface as a [`ArmProbeInterface`] object.
    fn get_arm_probe_interface(&mut self) -> Result<&mut dyn ArmDebugInterface, DebugProbeError>;

    /// Get this interface as a [`DapAccess`] object.
    fn get_dap_access(&mut self) -> Result<&mut dyn DapAccess, DebugProbeError>;

    /// Get the current value of the CSW reflected in this probe.
    fn generic_status(&mut self) -> Result<crate::architecture::arm::ap::CSW, ArmError>;

    /// Inform the probe of the [`CoreStatus`] of the chip/core attached to
    /// the probe.
    //
    // NOTE: this function should be infallible as it is usually only
    // a visual indication.
    fn update_core_status(&mut self, _state: CoreStatus) {}
}
