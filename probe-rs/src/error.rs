#![warn(missing_docs)]

use crate::architecture::arm::ArmError;
use crate::architecture::riscv::communication_interface::RiscvError;
use crate::architecture::xtensa::communication_interface::XtensaError;
use crate::config::RegistryError;
use crate::probe::DebugProbeError;

/// The overarching error type which contains all possible errors as variants.
#[derive(displaydoc::Display, thiserror::Error, Debug)]
#[ignore_extra_doc_attributes]
pub enum Error {
    /// An error with the usage of the probe occurred.
    Probe(#[from] DebugProbeError),
    /// An ARM specific error occurred.
    Arm(#[source] ArmError),
    /// A RISC-V specific error occurred.
    Riscv(#[source] RiscvError),
    /// An Xtensa specific error occurred.
    Xtensa(#[source] XtensaError),
    /// Probe could not be opened: {0}.
    UnableToOpenProbe(&'static str),
    /// Core {0} does not exist.
    CoreNotFound(usize),
    /// Unable to load specification for chip.
    ChipNotFound(#[from] RegistryError),
    /// An operation could not be performed because it lacked the permission to do so: {0}
    ///
    /// This can for example happen when the core is locked and needs to be erased to be unlocked.
    /// Then the correct permission needs to be given to automatically unlock the core to prevent accidental erases.
    MissingPermissions(String),
    /// A generic core (not architecture specific) error occurred.
    GenericCoreError(String),
    /// Error during core register operation: {0}.
    Register(String),
    /// This capability has not yet been implemented for this architecture: {0}
    ///
    /// Because of the large varieties of supported architectures, it is not always possible for
    /// a contributor to implement functionality for all of them. This allows us to
    /// implement new functionality on selected architectures first, and then add support for
    /// the other architectures later.
    NotImplemented(&'static str),
    /// {0}
    Other(#[from] anyhow::Error),
    // TODO: Errors below should be core specific
    /// A timeout occurred during an operation.
    Timeout,
    /// Attempted to access memory at address {address:X?} with improper alignment. (Required alignment: {alignment})
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
