//! Types and functions for interacting with target memory.

pub(crate) mod adi_v5_memory_interface;
pub mod romtable;

use crate::{memory::MemoryInterface, probe::DebugProbeError, CoreStatus};

use super::{
    ap_v1::memory_ap::MemoryAp,
    communication_interface::{Initialized, SwdSequence},
    ArmCommunicationInterface, ArmError, FullyQualifiedApAddress,
};
pub use romtable::{Component, ComponentId, CoresightComponent, PeripheralType};

/// An ArmMemoryInterface (ArmProbeInterface + MemoryAp)
pub trait ArmMemoryInterface: SwdSequence + ArmMemoryInterfaceShim {
    /// The underlying MemoryAp.
    fn ap(&mut self) -> &mut MemoryAp;

    /// The underlying MemoryAp address.
    fn fully_qualified_address(&self) -> FullyQualifiedApAddress;

    /// The underlying memory AP’s base address.
    fn base_address(&mut self) -> Result<u64, ArmError>;

    /// The underlying `ArmCommunicationInterface` if this is an `ArmCommunicationInterface`.
    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, DebugProbeError>;

    /// The underlying `ArmCommunicationInterface` and memory AP if the probe interface is an
    /// `ArmCommunicationInterface`.
    fn try_as_parts(
        &mut self,
    ) -> Result<(&mut ArmCommunicationInterface<Initialized>, &mut MemoryAp), DebugProbeError>;

    /// Inform the probe of the [`CoreStatus`] of the chip/core attached to
    /// the probe.
    //
    // NOTE: this function should be infallible as it is usually only
    // a visual indication.
    fn update_core_status(&mut self, state: CoreStatus) {
         let _ = self.get_arm_communication_interface()
            .map(|iface| iface.core_status_notification(state));
    }
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
