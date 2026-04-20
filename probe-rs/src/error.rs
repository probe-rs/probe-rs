use crate::architecture::arm::ArmError;
use crate::architecture::riscv::communication_interface::RiscvError;
use crate::architecture::xtensa::communication_interface::XtensaError;
use crate::config::RegistryError;
use crate::core::memory_mapped_registers::RegisterAddressOutOfBounds;
use crate::memory::{InvalidDataLengthError, MemoryNotAlignedError};
use crate::probe::DebugProbeError;

/// The overarching error type which contains all possible errors as variants.
#[derive(thiserror::Error, Debug, docsplay::Display)]
pub enum Error {
    /// An error with the usage of the probe occurred
    Probe(#[from] DebugProbeError),
    /// An ARM specific error occurred.
    Arm(#[source] ArmError),
    /// A RISC-V specific error occurred.
    Riscv(#[source] RiscvError),
    /// An Xtensa specific error occurred.
    Xtensa(#[source] XtensaError),
    /// Core {0} is not enabled.
    CoreDisabled(usize),
    /// Core {0} does not exist.
    CoreNotFound(usize),
    /// Memory access port {0} does not exist.
    MemoryAccessPortNotFound(usize),
    /// Unable to load specification for chip
    ChipNotFound(#[from] RegistryError),
    /// An operation was not performed because the required permissions were not given: {0}.
    ///
    /// This can for example happen when the core is locked and needs to be erased to be unlocked.
    /// Then the correct permission needs to be given to automatically unlock the core to prevent accidental erases.
    #[ignore_extra_doc_attributes]
    MissingPermissions(String),
    /// An error that is not architecture specific occurred: {0}
    GenericCoreError(String),
    /// Errors accessing core register: {0}
    Register(String),

    /// Error calculating the address of a register
    #[error(transparent)]
    RegisterAddressOutOfBounds(#[from] RegisterAddressOutOfBounds),
    /// The {0} capability has not yet been implemented for this architecture.
    ///
    /// Because of the large varieties of supported architectures, it is not always possible for
    /// a contributor to implement functionality for all of them. This allows us to
    /// implement new functionality on selected architectures first, and then add support for
    /// the other architectures later.
    NotImplemented(&'static str),
    /// Some uncategorized error occurred.
    #[display("{0}")]
    Other(String),
    /// A timeout occurred.
    // TODO: Errors below should be core specific
    Timeout,
    /// Memory access to address {0.address:#X?} was not aligned to {0.alignment} bytes.
    #[error(transparent)]
    MemoryNotAligned(#[from] MemoryNotAlignedError),
    /// The data buffer had an invalid length.
    #[error(transparent)]
    InvalidDataLength(#[from] InvalidDataLengthError),
    /// Failed to write CPU register {register}.
    WriteRegister {
        /// The name of the register that was tried to be written.
        register: String,
        /// The source error of this error.
        source: Box<dyn std::error::Error + 'static + Send + Sync>,
    },
    /// Failed to read CPU register {register}.
    ReadRegister {
        /// The name of the register that was tried to be read.
        register: String,
        /// The source error of this error.
        source: Box<dyn std::error::Error + 'static + Send + Sync>,
    },

    /// Error during breakpoint configuration
    BreakpointOperation(#[from] BreakpointError),
}

/// Errors that occur during breakpoint configuration
#[derive(thiserror::Error, Debug, docsplay::Display)]
pub enum BreakpointError {
    /// No breakpoint found at address {0:#010x}
    NotFound(u64),
}

impl From<ArmError> for Error {
    fn from(value: ArmError) -> Self {
        match value {
            ArmError::Timeout => Error::Timeout,
            ArmError::MemoryNotAligned(e) => Error::MemoryNotAligned(e),
            ArmError::InvalidDataLength(e) => Error::InvalidDataLength(e),
            other => Error::Arm(other),
        }
    }
}
