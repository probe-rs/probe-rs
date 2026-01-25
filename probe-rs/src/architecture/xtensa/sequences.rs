//! Debug sequences to operate special requirements Xtensa targets.

use std::{fmt::Debug, sync::Arc, time::Duration};

use crate::architecture::xtensa::Xtensa;
use crate::architecture::xtensa::communication_interface::XtensaCommunicationInterface;
use crate::semihosting::{SemihostingCommand, UnknownCommandDetails};

/// A interface to operate debug sequences for Xtensa targets.
///
/// Should be implemented on a custom handle for chips that require special sequence code.
pub trait XtensaDebugSequence: Send + Sync + Debug {
    /// Executed when the probe establishes a connection to the target.
    fn on_connect(
        &self,
        _interface: &mut XtensaCommunicationInterface,
    ) -> Result<(), crate::Error> {
        Ok(())
    }

    /// Executed when the target is halted.
    fn on_halt(&self, _interface: &mut XtensaCommunicationInterface) -> Result<(), crate::Error> {
        Ok(())
    }

    /// Executes a system-wide reset without debug domain (or warm-reset that preserves debug connection) via software mechanisms.
    fn reset_system_and_halt(
        &self,
        interface: &mut XtensaCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        interface.reset_and_halt(timeout)?;

        Ok(())
    }

    /// Attempts to handle target-dependent semihosting commands.
    ///
    /// Returns `Ok(Some(command))` if the command was not fully handled, `Ok(None)`
    /// if the command was fully handled.
    fn on_unknown_semihosting_command(
        &self,
        _interface: &mut Xtensa,
        details: UnknownCommandDetails,
    ) -> Result<Option<SemihostingCommand>, crate::Error> {
        Ok(Some(SemihostingCommand::Unknown(details)))
    }

    /// This ARM sequence is called if an image was flashed to RAM directly. It should perform the
    /// necessary preparation to run that image on the core with the ID passed to the function.
    ///
    /// The core should already be `reset_and_halt`ed right before this call.
    fn prepare_running_on_ram(
        &self,
        _session: &mut crate::Session,
        _vector_table_addr: u64,
        _core_id: usize,
    ) -> Result<(), crate::Error> {
        Err(crate::Error::NotImplemented(
            "RAM running on Xtensa targets",
        ))
    }
}

/// The default sequences that is used for Xtensa chips that do not specify a specific sequence.
#[derive(Debug)]
pub struct DefaultXtensaSequence(pub(crate) ());

impl DefaultXtensaSequence {
    /// Creates a new default RISC-V debug sequence.
    pub fn create() -> Arc<dyn XtensaDebugSequence> {
        Arc::new(Self(()))
    }
}

impl XtensaDebugSequence for DefaultXtensaSequence {}
