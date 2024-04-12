use std::{fmt::Debug, sync::Arc};

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

    /// Configure the target to stop code execution after a reset. After this, the core will halt when it comes
    /// out of reset.
    fn reset_catch_set(
        &self,
        interface: &mut XtensaCommunicationInterface,
    ) -> Result<(), XtensaError> {
        interface.xdm.halt_on_reset(true);
        Ok(())
    }

    /// Free hardware resources allocated by ResetCatchSet.
    fn reset_catch_clear(
        &self,
        interface: &mut XtensaCommunicationInterface,
    ) -> Result<(), XtensaError> {
        interface.xdm.halt_on_reset(false);
        Ok(())
    }

    /// Executes a system-wide reset without debug domain (or warm-reset that preserves debug connection) via software mechanisms.
    fn reset_system(
        &self,
        interface: &mut XtensaCommunicationInterface,
    ) -> Result<(), XtensaError> {
        interface.reset()?;

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
