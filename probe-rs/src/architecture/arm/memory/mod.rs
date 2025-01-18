//! Types and functions for interacting with target memory.

pub(crate) mod adi_v5_memory_interface;
pub mod romtable;

use crate::{memory::MemoryInterface, probe::DebugProbeError, CoreStatus};

use super::{
    ap::memory_ap::{registers, MemoryAp},
    communication_interface::SwdSequence,
    ArmError, ArmProbeInterface, DapAccess,
};
pub use romtable::{Component, ComponentId, CoresightComponent, PeripheralType};

/// An ArmMemoryInterface (ArmProbeInterface + MemoryAp)
pub trait ArmMemoryInterface: ArmMemoryInterfaceShim {
    /// The underlying MemoryAp.
    fn ap(&mut self) -> &mut MemoryAp;

    /// The underlying memory AP’s base address.
    fn base_address(&mut self) -> Result<u64, ArmError>;

    /// Get this interface as a SwdSequence object.
    fn get_swd_sequence(&mut self) -> Result<&mut dyn SwdSequence, DebugProbeError>;

    /// Get this interface as a [`ArmProbeInterface`] object.
    fn get_arm_probe_interface(&mut self) -> Result<&mut dyn ArmProbeInterface, DebugProbeError>;

    /// Get this interface as a [`DapAccess`] object.
    fn get_dap_access(&mut self) -> Result<&mut dyn DapAccess, DebugProbeError>;

    /// Get the current value of the CSW reflected in this probe.
    fn generic_status(&mut self) -> Result<registers::CSW, ArmError>;

    /// Inform the probe of the [`CoreStatus`] of the chip/core attached to
    /// the probe.
    //
    // NOTE: this function should be infallible as it is usually only
    // a visual indication.
    fn update_core_status(&mut self, _state: CoreStatus) {}
}

/// Implementation detail to allow trait upcasting-like behaviour.
//
// TODO: replace with trait upcasting once stable
pub trait ArmMemoryInterfaceShim: MemoryInterface<ArmError> {
    /// Returns a reference to the underlying `MemoryInterface`.
    // TODO: replace with trait upcasting once stable
    fn as_memory_interface(&self) -> &dyn MemoryInterface<ArmError>;

    /// Returns a mutable reference to the underlying `MemoryInterface`.
    // TODO: replace with trait upcasting once stable
    fn as_memory_interface_mut(&mut self) -> &mut dyn MemoryInterface<ArmError>;
}

impl<T> ArmMemoryInterfaceShim for T
where
    T: ArmMemoryInterface,
{
    fn as_memory_interface(&self) -> &dyn MemoryInterface<ArmError> {
        self
    }

    fn as_memory_interface_mut(&mut self) -> &mut dyn MemoryInterface<ArmError> {
        self
    }
}
