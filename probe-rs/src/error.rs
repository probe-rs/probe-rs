#![warn(missing_docs)]

use crate::architecture::arm::ArmError;
use crate::architecture::riscv::communication_interface::RiscvError;
use crate::DebugProbeError;
use crate::{architecture::arm::ap::AccessPortError, config::RegistryError};

/// The overarching error type which contains all possible errors as variants.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// An error in the probe driver occurred.
    #[error("An error with the usage of the probe occurred")]
    Probe(#[from] DebugProbeError),
    /// An architecture specific error occurred. Some error that is only possible with the current architecture.
    #[error("A core architecture specific error occurred")]
    ArchitectureSpecific(#[from] Box<dyn std::error::Error + Send + Sync>),
    /// An ARM specific error occured.
    #[error("A ARM specific error occured.")]
    Arm(#[from] ArmError),
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
    /// The requested feature requires one of the architectures specified by this error.
    #[error("This feature requires one of the following architectures: {0:?}")]
    ArchitectureRequired(&'static [&'static str]),
    /// An operation was not performed because the required permissions were not given.
    ///
    /// This can for example happen when the core is locked and needs to be erased to be unlocked.
    /// Then the correct permission needs to be given to automatically unlock the core to prevent accidental erases.
    #[error("An operation could not be performed because it lacked the permission to do so: {0}")]
    MissingPermissions(String),
    /// Any other error occurred.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Error {
    /// Create an architecture specific error and automatically box its source.
    pub fn architecture_specific(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::ArchitectureSpecific(Box::new(e))
    }
}

impl From<AccessPortError> for Error {
    fn from(err: AccessPortError) -> Self {
        Error::architecture_specific(err)
    }
}
