//! Xtensa Debug Module Communication

use crate::{DebugProbeError, probe::JTAGAccess};

use super::xdm::{Xdm, Error as XdmError};

/// Possible Xtensa errors
#[derive(thiserror::Error, Debug)]
pub enum XtensaError {
    /// An error originating from the DebugProbe
    #[error("Debug Probe Error")]
    DebugProbe(#[from] DebugProbeError),
    /// Xtensa debug module error
    #[error("Xtensa debug module error")]
    XdmError(XdmError),
}

/// A interface that implements controls for RISC-V cores.
#[derive(Debug)]
pub struct XtensaCommunicationInterface {
    /// The Xtensa debug module
    xdm: Xdm,
    // state: RiscvCommunicationInterfaceState,
}

impl XtensaCommunicationInterface {
    /// Create the Xtensa communication interface using the underlying probe driver
    pub fn new(probe: Box<dyn JTAGAccess>) -> Result<Self, (Box<dyn JTAGAccess>, DebugProbeError)> {
        let xdm = Xdm::new(probe).map_err(|(probe, e)| match e {
            XtensaError::DebugProbe(err) => (probe, err),
            other_error => (
                probe,
                DebugProbeError::Other(other_error.into()),
            ),
        })?;

        let s = Self { xdm };

        Ok(s)
    }
}
