use crate::architecture::riscv::communication_interface::RiscvError;
use crate::probe::{CommandResult, DebugProbeError, DeferredResultIndex};
use std::fmt;
use std::time::Duration;

pub trait DtmAccess: Send + fmt::Debug {
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
        address: u64,
        value: u32,
    ) -> Result<Option<DeferredResultIndex>, RiscvError>;

    /// Schedule a read from an address on the `dmi` bus.
    fn schedule_read(&mut self, address: u64) -> Result<DeferredResultIndex, RiscvError>;

    /// Read an address on the `dmi` bus. If a busy value is returned, the access is
    /// retried until the transfer either succeeds, or the timeout expires.
    fn read_with_timeout(&mut self, address: u64, timeout: Duration) -> Result<u32, RiscvError>;

    /// Write an address to the `dmi` bus. If a busy value is returned, the access is
    /// retried until the transfer either succeeds, or the timeout expires.
    ///
    /// Returns None if the underlying protocol does not return the value on write
    fn write_with_timeout(
        &mut self,
        address: u64,
        value: u32,
        timeout: Duration,
    ) -> Result<Option<u32>, RiscvError>;

    /// Returns an idcode used for chip detection
    fn read_idcode(&mut self) -> Result<Option<u32>, DebugProbeError>;
}
