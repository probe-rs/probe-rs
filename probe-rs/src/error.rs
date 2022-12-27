#![warn(missing_docs)]

use crate::architecture::arm::ArmError;
use crate::architecture::riscv::communication_interface::RiscvError;
use crate::config::RegistryError;
use crate::DebugProbeError;

/// The overarching error type which contains all possible errors as variants.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// An error in the probe driver occurred.
    #[error("An error with the usage of the probe occurred")]
    Probe(#[from] DebugProbeError),
    /// An ARM specific error occured.
    #[error("A ARM specific error occured.")]
    Arm(#[source] ArmError),
    /// A RISCV specific error occured.
    #[error("A RISCV specific error occured.")]
    Riscv(#[source] RiscvError),
    /// The probe could not be opened.
    #[error("Probe could not be opened: {0}")]
    UnableToOpenProbe(&'static str),
    /// The core with given ID does not exist.
    #[error("Core {0} does not exist")]
    CoreNotFound(usize),
    /// The given chip does not exist.
    #[error("Unable to load specification for chip")]
    ChipNotFound(#[from] RegistryError),
    /// An operation was not performed because the required permissions were not given.
    ///
    /// This can for example happen when the core is locked and needs to be erased to be unlocked.
    /// Then the correct permission needs to be given to automatically unlock the core to prevent accidental erases.
    #[error("An operation could not be performed because it lacked the permission to do so: {0}")]
    MissingPermissions(String),
    /// Any other error occurred.
    #[error(transparent)]
    Other(#[from] anyhow::Error),

    // TODO: Errors below should be core specific
    /// A timeout occured during an operation
    #[error("A timeout occured.")]
    Timeout,

    /// Unaligned memory access
    #[error("Alignment error")]
    MemoryNotAligned {
        /// The address of the register.
        address: u64,
        /// The required alignment in bytes (address increments).
        alignment: usize,
    },
}

impl From<ArmError> for Error {
    fn from(value: ArmError) -> Self {
        match value {
            ArmError::Timeout => Error::Timeout,
            ArmError::MemoryNotAligned { address, alignment } => {
                Error::MemoryNotAligned { address, alignment }
            }
            other => Error::Arm(other),
        }
    }
}
