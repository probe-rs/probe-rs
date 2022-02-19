//! Debug sequences to operate special requirements RISC-V targets.

use super::communication_interface::RiscvCommunicationInterface;
use std::sync::Arc;

pub mod esp32c3;

/// A interface to operate debug sequences for RISC-V targets.
///
/// Should be implemented on a custom handle for chips that require special sequence code.
pub trait RiscvDebugSequence: Send + Sync {
    /// Executed when the probe establishes a connection to the target.
    fn on_connect(&self, _interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        Ok(())
    }
}

/// The default sequences that is used for RISC-V chips that do not specify a specific sequence.
pub struct DefaultRiscvSequence(pub(crate) ());

impl DefaultRiscvSequence {
    /// Creates a new default RISC-V debug sequence.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self(()))
    }
}

impl RiscvDebugSequence for DefaultRiscvSequence {}
