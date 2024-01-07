//! Debug sequences to operate special requirements RISC-V targets.

use super::communication_interface::RiscvCommunicationInterface;
use super::{read_csr, write_csr, Dcsr};
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

    /// Detects the flash size of the target.
    fn detect_flash_size(
        &self,
        _interface: &mut RiscvCommunicationInterface,
    ) -> Result<Option<usize>, crate::Error> {
        Ok(None)
    }

    /// Enable debugging for a core (hart).
    ///
    /// This should enable software breakpoints.
    fn debug_core_start(
        &self,
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<(), crate::Error> {
        debug_on_sw_breakpoint(interface, true)
    }

    /// Disable debugging for a core (hart).
    ///
    /// This should disable software breakpoints.
    fn debug_core_stop(
        &self,
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<(), crate::Error> {
        debug_on_sw_breakpoint(interface, false)
    }
}

fn debug_on_sw_breakpoint(
    interface: &mut RiscvCommunicationInterface,
    enabled: bool,
) -> Result<(), crate::Error> {
    let mut dcsr = Dcsr(read_csr(interface, 0x7b0)?);

    dcsr.set_ebreakm(enabled);
    dcsr.set_ebreaks(enabled);
    dcsr.set_ebreaku(enabled);

    write_csr(interface, 0x7b0, dcsr.0).map_err(|e| e.into())
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
