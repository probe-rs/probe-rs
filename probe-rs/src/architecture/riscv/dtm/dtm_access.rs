use crate::MemoryInterface;
use crate::architecture::arm::ArmError;
use crate::architecture::riscv::communication_interface::RiscvError;
use crate::probe::{CommandResult, DebugProbeError, DeferredResultIndex};
use std::time::Duration;

/// Addresses used in the Debug Module (DM)
///
/// Maximum address width is 32 bits, according to the RISC-V Debug Specification.
///
/// Each address on the `dmi` bus is represented by a `DmAddress` struct,
/// and represents a single register. This means that 0x0 and 0x1 are valid addresses,
/// and refer to two different registers.
#[derive(Debug, Clone, Copy)]
pub struct DmAddress(pub u32);

pub trait DtmAccess: Send {
    /// Perform interface-specific initialisation upon attaching.
    fn init(&mut self) -> Result<(), RiscvError> {
        Ok(())
    }

    /// Asserts a reset of the physical pins
    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError>;

    /// Deasserts a reset of the physical pins
    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError>;

    /// Clear the sticky error state, if applicable
    fn clear_error_state(&mut self) -> Result<(), RiscvError>;

    /// Read previously scheduled `dmi` register accesses
    fn read_deferred_result(
        &mut self,
        index: DeferredResultIndex,
    ) -> Result<CommandResult, RiscvError>;

    /// Execute scheduled dmi accesses
    fn execute(&mut self) -> Result<(), RiscvError>;

    /// Schedule a write to an address on the `dmi` bus.
    ///
    /// Returns None if the underlying transport protocol does
    /// not return the value at the address on write
    fn schedule_write(
        &mut self,
        address: DmAddress,
        value: u32,
    ) -> Result<Option<DeferredResultIndex>, RiscvError>;

    /// Schedule a read from an address on the `dmi` bus.
    fn schedule_read(&mut self, address: DmAddress) -> Result<DeferredResultIndex, RiscvError>;

    /// Read an address on the `dmi` bus. If a busy value is returned, the access is
    /// retried until the transfer either succeeds, or the timeout expires.
    fn read_with_timeout(
        &mut self,
        address: DmAddress,
        timeout: Duration,
    ) -> Result<u32, RiscvError>;

    /// Write an address to the `dmi` bus. If a busy value is returned, the access is
    /// retried until the transfer either succeeds, or the timeout expires.
    ///
    /// Returns None if the underlying protocol does not return the value on write
    fn write_with_timeout(
        &mut self,
        address: DmAddress,
        value: u32,
        timeout: Duration,
    ) -> Result<Option<u32>, RiscvError>;

    /// Returns an idcode used for chip detection
    fn read_idcode(&mut self) -> Result<Option<u32>, DebugProbeError>;

    // TODO: Put in proper error type
    fn memory_interface<'m>(
        &'m mut self,
    ) -> Result<&'m mut dyn MemoryInterface<ArmError>, DebugProbeError>;
}
