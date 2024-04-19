use std::{fmt::Debug, sync::Arc, time::Duration};

use crate::architecture::xtensa::communication_interface::{
    ProgramStatus, XtensaCommunicationInterface, XtensaError,
};

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

    /// Detects the flash size of the target.
    fn detect_flash_size(
        &self,
        _interface: &mut XtensaCommunicationInterface,
    ) -> Result<Option<usize>, crate::Error> {
        Ok(None)
    }

    /// Executes a system-wide reset without debug domain (or warm-reset that preserves debug connection) via software mechanisms.
    fn reset_system_and_halt(
        &self,
        interface: &mut XtensaCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), XtensaError> {
        interface.reset_and_halt(timeout)?;

        // TODO: this is only necessary to run code, so this might not be the best place
        // Make sure the CPU is in a known state and is able to run code we download.
        interface.write_register({
            let mut ps = ProgramStatus(0);
            ps.set_intlevel(1);
            ps.set_user_mode(true);
            ps.set_woe(true);
            ps
        })?;

        Ok(())
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
