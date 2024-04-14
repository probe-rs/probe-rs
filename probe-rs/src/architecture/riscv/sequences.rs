//! Debug sequences to operate special requirements RISC-V targets.

use crate::architecture::riscv::communication_interface::RiscvError;
use crate::architecture::riscv::Dmcontrol;

use super::communication_interface::RiscvCommunicationInterface;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

/// A interface to operate debug sequences for RISC-V targets.
///
/// Should be implemented on a custom handle for chips that require special sequence code.
pub trait RiscvDebugSequence: Send + Sync + Debug {
    /// Executed when the probe establishes a connection to the target.
    fn on_connect(&self, _interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        Ok(())
    }

    /// Detects the flash size of the target.
    fn detect_flash_size(
        &self,
        _interface: &mut RiscvCommunicationInterface,
    ) -> Result<Option<usize>, crate::Error> {
        Ok(None)
    }

    /// Configure the target to stop code execution after a reset. After this, the core will halt when it comes
    /// out of reset.
    fn reset_catch_set(
        &self,
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<(), RiscvError> {
        if !interface.supports_reset_halt_req()? {
            return Err(RiscvError::ResetHaltRequestNotSupported);
        }

        let mut dmcontrol: Dmcontrol = interface.read_dm_register()?;

        dmcontrol.set_dmactive(true);
        dmcontrol.set_resethaltreq(true);

        interface.write_dm_register(dmcontrol)?;

        Ok(())
    }

    /// Free hardware resources allocated by ResetCatchSet.
    fn reset_catch_clear(
        &self,
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<(), RiscvError> {
        if !interface.supports_reset_halt_req()? {
            return Err(RiscvError::ResetHaltRequestNotSupported);
        }

        let mut dmcontrol: Dmcontrol = interface.read_dm_register()?;

        dmcontrol.set_dmactive(true);
        dmcontrol.set_clrresethaltreq(true);

        interface.write_dm_register(dmcontrol)?;

        Ok(())
    }

    /// Executes a system-wide reset without debug domain (or warm-reset that preserves debug connection) via software mechanisms.
    fn reset_system_and_halt(
        &self,
        interface: &mut RiscvCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        interface.reset_hart_and_halt(timeout)?;
        Ok(())
    }
}

/// The default sequences that is used for RISC-V chips that do not specify a specific sequence.
#[derive(Debug)]
pub struct DefaultRiscvSequence(pub(crate) ());

impl DefaultRiscvSequence {
    /// Creates a new default RISC-V debug sequence.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self(()))
    }
}

impl RiscvDebugSequence for DefaultRiscvSequence {}
