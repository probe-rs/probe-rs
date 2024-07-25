use std::{fmt::Debug, sync::Arc, time::Duration};

use crate::architecture::xtensa::communication_interface::XtensaCommunicationInterface;
use crate::{Error, Session};

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
    fn detect_flash_size(&self, _session: &mut Session) -> Result<Option<usize>, crate::Error> {
        Ok(None)
    }

    /// Executes a system-wide reset without debug domain (or warm-reset that preserves debug connection) via software mechanisms.
    fn reset_and_halt_system(&self, session: &mut Session, timeout: Duration) -> Result<(), Error> {
        session.list_cores().into_iter().try_for_each(|(n, _)| {
            session
                .core(n)
                .and_then(|mut core| core.reset_and_halt(timeout))
                .map(|_| ())
        })
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
