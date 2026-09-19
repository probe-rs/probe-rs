//! Types and functions for interacting with target memory.

mod adi_memory_interface;
pub mod romtable;

pub(crate) use adi_memory_interface::ADIMemoryInterface;

use crate::{CoreStatus, memory::MemoryInterface, probe::DebugProbeError};

use super::{ArmDebugInterface, ArmError, FullyQualifiedApAddress};
use crate::memory::{Operation, OperationKind};
pub use romtable::{Component, ComponentId, CoresightComponent, PeripheralType, RomTable};

/// Trait for accessing memory behind a memory access port,
/// as defined in the ARM Debug Interface Specification.
pub trait ArmMemoryInterface: MemoryInterface<ArmError> {
    /// Perform these memory operations in as few probe transactions as possible.
    ///
    /// The operations run in order and stop at the first failure. Each one that completed records
    /// `Ok` in [`Operation::result`]; the one that failed and everything after it record nothing,
    /// and the error comes back from this call. [`MemoryInterface::execute_memory_operations`]
    /// returns nothing, so it has to record the failure instead.
    ///
    /// This is for sequences where a write decides what the next read returns: select a debug
    /// register, then read the value it selected.
    ///
    /// The default performs them one at a time.
    fn execute_operations(&mut self, operations: &mut [Operation<'_>]) -> Result<(), ArmError> {
        for operation in operations.iter_mut() {
            let address = operation.address;
            let outcome = match &mut operation.operation {
                OperationKind::Read(data) => self.read(address, data),
                OperationKind::Read8(data) => self.read_8(address, data),
                OperationKind::Read16(data) => self.read_16(address, data),
                OperationKind::Read32(data) => self.read_32(address, data),
                OperationKind::Read64(data) => self.read_64(address, data),
                OperationKind::Write(data) => self.write(address, data),
                OperationKind::Write8(data) => self.write_8(address, data),
                OperationKind::Write16(data) => self.write_16(address, data),
                OperationKind::Write32(data) => self.write_32(address, data),
                OperationKind::Write64(data) => self.write_64(address, data),
                OperationKind::WriteWord8(data) => self.write_word_8(address, *data),
                OperationKind::WriteWord16(data) => self.write_word_16(address, *data),
                OperationKind::WriteWord32(data) => self.write_word_32(address, *data),
                OperationKind::WriteWord64(data) => self.write_word_64(address, *data),
            };
            outcome?;
            operation.result = Some(Ok(()));
        }
        Ok(())
    }

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
