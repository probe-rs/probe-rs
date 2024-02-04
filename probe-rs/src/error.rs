#![warn(missing_docs)]

use crate::architecture::arm::ArmError;
use crate::architecture::riscv::communication_interface::RiscvError;
use crate::architecture::xtensa::communication_interface::XtensaError;
use crate::config::RegistryError;
use crate::probe::DebugProbeError;

/// The overarching error type which contains all possible errors as variants.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// An error in the probe driver occurred.
    #[error("An error with the usage of the probe occurred")]
    Probe(#[from] DebugProbeError),
    /// An ARM specific error occurred.
    #[error("An ARM specific error occurred.")]
    Arm(#[source] ArmError),
    /// A RISC-V specific error occurred.
    #[error("A RISC-V specific error occurred.")]
    Riscv(#[source] RiscvError),
    /// An Xtensa specific error occurred.
    #[error("An Xtensa specific error occurred.")]
    Xtensa(#[source] XtensaError),
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
    /// An error that is not architecture specific occurred.
    #[error("A generic core (not architecture specific) error occurred.")]
    GenericCoreError(String),
    /// Errors related to the handling of core registers inside probe-rs .
    #[error("Register error: {0}")]
    Register(String),
    /// The variant of the function you called is not yet implemented.
    /// Because of the large varieties of supported architectures, it is not always possible for
    /// a contributor to implement functionality for all of them. This allows us to
    /// implement new functionality on selected architectures first, and then add support for
    /// the other architectures later.
    #[error("This capability has not yet been implemented for this architecture: {0}")]
    NotImplemented(&'static str),
    /// Any other error occurred.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
    // TODO: Errors below should be core specific
    /// A timeout occurred during an operation
    #[error("A timeout occurred.")]
    Timeout,
    /// Unaligned memory access
    #[error("Alignment error")]
    MemoryNotAligned {
        /// The address of the register.
        address: u64,
        /// The required alignment in bytes (address increments).
        alignment: usize,
    },
    /// The current Config does not enable debugging for the given core.
    #[error("Debugging is not enabled for the core with id {0} with the current Config")]
    DebugNotEnabled(usize),
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
