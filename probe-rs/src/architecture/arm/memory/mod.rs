//! Types and functions for interacting with target memory.

mod adi_v5_memory_interface;
pub mod romtable;

pub(crate) use adi_v5_memory_interface::ADIMemoryInterface;

use crate::{memory::MemoryInterface, probe::DebugProbeError, CoreStatus};

use super::{
    ap_v1, ap_v2, communication_interface::SwdSequence, ArmError, ArmProbeInterface, DapAccess,
    FullyQualifiedApAddress,
};
pub use romtable::{Component, ComponentId, CoresightComponent, PeripheralType, RomTable};

/// A generic status indication for an AP.
pub enum Status {
    /// A CSW associated with an APv1 (ADIv5) access port.
    V1(ap_v1::memory_ap::registers::CSW),
    /// A CSW associated with an APv2 (ADIv6) access port.
    V2(ap_v2::registers::CSW),
}

impl Status {
    /// Check if the AP is enabled.
    pub fn enabled(&self) -> bool {
        match self {
            Self::V1(csw) => csw.DeviceEn,
            Self::V2(csw) => csw.DeviceEn,
        }
    }
}

impl From<ap_v1::memory_ap::registers::CSW> for Status {
    fn from(csw: ap_v1::memory_ap::registers::CSW) -> Self {
        Self::V1(csw)
    }
}

impl From<ap_v2::registers::CSW> for Status {
    fn from(csw: ap_v2::registers::CSW) -> Self {
        Self::V2(csw)
    }
}

/// An ArmMemoryInterface (ArmProbeInterface + MemoryAp)
pub trait ArmMemoryInterface: ArmMemoryInterfaceShim {
    /// The underlying MemoryAp address.
    fn fully_qualified_address(&self) -> FullyQualifiedApAddress;

    /// The underlying memory AP’s base address.
    fn base_address(&mut self) -> Result<u64, ArmError>;

    /// Get this interface as a SwdSequence object.
    fn get_swd_sequence(&mut self) -> Result<&mut dyn SwdSequence, DebugProbeError>;

    /// Get this interface as a [`ArmProbeInterface`] object.
    fn get_arm_probe_interface(&mut self) -> Result<&mut dyn ArmProbeInterface, DebugProbeError>;

    /// Get this interface as a [`DapAccess`] object.
    fn get_dap_access(&mut self) -> Result<&mut dyn DapAccess, DebugProbeError>;

    /// Get the current value of the CSW reflected in this probe.
    fn generic_status(&mut self) -> Result<Status, ArmError>;

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
