//! Debug sequences to operate special requirements RISC-V targets.

use crate::architecture::esp32::EspDebugSequence;

use super::communication_interface::RiscvCommunicationInterface;
use std::fmt::Debug;
use std::sync::Arc;

/// A interface to operate debug sequences for RISC-V targets.
///
/// Should be implemented on a custom handle for chips that require special sequence code.
pub trait RiscvDebugSequence: Send + Sync + Debug {
    /// Executed when the probe establishes a connection to the target.
    fn on_connect(&self, _interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        Ok(())
    }

    /// Returns the ESP-specific debug sequences if available.
    fn as_esp_sequence(
        &self,
    ) -> Option<&dyn EspDebugSequence<Interface = RiscvCommunicationInterface>> {
        None
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
