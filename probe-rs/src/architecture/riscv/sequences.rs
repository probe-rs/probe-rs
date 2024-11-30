//! Debug sequences to operate special requirements RISC-V targets.

use crate::Session;
use crate::architecture::riscv::communication_interface::RiscvError;
use crate::architecture::riscv::{Dmcontrol, Riscv32};
use crate::semihosting::{SemihostingCommand, UnknownCommandDetails};

use super::communication_interface::RiscvCommunicationInterface;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

/// A interface to operate debug sequences for RISC-V targets.
///
/// Should be implemented on a custom handle for chips that require special sequence code.
#[async_trait::async_trait(?Send)]
pub trait RiscvDebugSequence: Send + Sync + Debug {
    /// Executed when the probe establishes a connection to the target.
    async fn on_connect(
        &self,
        _interface: &mut RiscvCommunicationInterface,
    ) -> Result<(), crate::Error> {
        Ok(())
    }

    /// Executed when the target is halted.
    async fn on_halt(
        &self,
        _interface: &mut RiscvCommunicationInterface,
    ) -> Result<(), crate::Error> {
        Ok(())
    }

    /// Detects the flash size of the target.
    async fn detect_flash_size(
        &self,
        _session: &mut Session,
    ) -> Result<Option<usize>, crate::Error> {
        Ok(None)
    }

    /// Configure the target to stop code execution after a reset. After this, the core will halt when it comes
    /// out of reset.
    async fn reset_catch_set(
        &self,
        interface: &mut RiscvCommunicationInterface<'_>,
    ) -> Result<(), RiscvError> {
        if !interface.supports_reset_halt_req().await? {
            return Err(RiscvError::ResetHaltRequestNotSupported);
        }

        let mut dmcontrol: Dmcontrol = interface.read_dm_register().await?;

        dmcontrol.set_dmactive(true);
        dmcontrol.set_resethaltreq(true);

        interface.write_dm_register(dmcontrol).await?;

        Ok(())
    }

    /// Free hardware resources allocated by ResetCatchSet.
    async fn reset_catch_clear(
        &self,
        interface: &mut RiscvCommunicationInterface<'_>,
    ) -> Result<(), RiscvError> {
        if !interface.supports_reset_halt_req().await? {
            return Err(RiscvError::ResetHaltRequestNotSupported);
        }

        let mut dmcontrol: Dmcontrol = interface.read_dm_register().await?;

        dmcontrol.set_dmactive(true);
        dmcontrol.set_clrresethaltreq(true);

        interface.write_dm_register(dmcontrol).await?;

        Ok(())
    }

    /// Executes a system-wide reset without debug domain (or warm-reset that preserves debug connection) via software mechanisms.
    async fn reset_system_and_halt(
        &self,
        interface: &mut RiscvCommunicationInterface<'_>,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        interface.reset_hart_and_halt(timeout).await?;
        Ok(())
    }

    /// Attempts to handle target-dependent semihosting commands.
    ///
    /// Returns `Ok(Some(command))` if the command was not fully handled, `Ok(None)`
    /// if the command was fully handled.
    async fn on_unknown_semihosting_command(
        &self,
        _interface: &mut Riscv32,
        details: UnknownCommandDetails,
    ) -> Result<Option<SemihostingCommand>, crate::Error> {
        Ok(Some(SemihostingCommand::Unknown(details)))
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
