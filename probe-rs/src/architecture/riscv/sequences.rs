//! Debug sequences to operate special requirements RISC-V targets.

use crate::Session;
use std::fmt::Debug;
use std::sync::Arc;

/// A interface to operate debug sequences for RISC-V targets.
///
/// Should be implemented on a custom handle for chips that require special sequence code.
pub trait RiscvDebugSequence: Send + Sync + Debug {
    /// Executed when the probe establishes a connection to the target.
    fn on_connect(&self, _session: &mut Session) -> Result<(), crate::Error> {
        Ok(())
    }

    /// Detects the flash size of the target.
    fn detect_flash_size(
        &self,
        _session: &mut Session,
    ) -> Result<Option<usize>, crate::Error> {
        Ok(None)
    }

    /// Perform a SoC wide reset
    fn soc_reset(&self, _session: &mut Session) -> Result<(), crate::Error> {
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
