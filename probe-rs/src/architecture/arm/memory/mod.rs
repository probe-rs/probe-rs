//! Types and functions for interacting with target memory.

pub(crate) mod adi_v5_memory_interface;
pub mod romtable;

use crate::{memory::MemoryInterface, probe::DebugProbeError, CoreStatus};

use super::{
    ap::memory_ap::MemoryAp,
    communication_interface::{Initialized, SwdSequence},
    ArmCommunicationInterface, ArmError,
};
pub use romtable::{Component, ComponentId, CoresightComponent, PeripheralType};

/// An ArmMemoryInterface (ArmProbeInterface + MemoryAp)
pub trait ArmMemoryInterface: SwdSequence + MemoryInterface<ArmError> {
    /// The underlying MemoryAp.
    fn ap(&mut self) -> MemoryAp;

    /// The underlying `ArmCommunicationInterface` if this is an `ArmCommunicationInterface`.
    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, DebugProbeError>;

    /// Inform the probe of the [`CoreStatus`] of the chip/core attached to
    /// the probe.
    //
    // NOTE: this function should be infallible as it is usually only
    // a visual indication.
    fn update_core_status(&mut self, state: CoreStatus) {
        self.get_arm_communication_interface()
            .map(|iface| iface.core_status_notification(state))
            .ok();
    }
}
