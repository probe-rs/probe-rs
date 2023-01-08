//! All the interface bits for ARM.

pub mod ap;
pub(crate) mod communication_interface;
pub mod component;
pub(crate) mod core;
pub mod dp;
pub mod memory;
pub mod sequences;
pub mod swo;
mod traits;

pub use communication_interface::{
    ApInformation, ArmChipInfo, ArmCommunicationInterface, DapError, MemoryApInformation, Register,
};
pub use swo::{SwoAccess, SwoConfig, SwoMode, SwoReader};
pub use traits::*;

use crate::DebugProbeError;

use self::ap::AccessPort;
use self::ap::AccessPortError;
use self::armv7a::Armv7aError;
use self::armv8a::Armv8aError;
use self::communication_interface::RegisterParseError;
pub use self::core::armv6m;
pub use self::core::armv7a;
pub use self::core::armv7m;
pub use self::core::armv8a;
pub use self::core::armv8m;
pub use self::core::Dump;
use self::dp::DebugPortError;
use self::memory::romtable::RomTableError;
use self::sequences::ArmDebugSequenceError;

pub use communication_interface::ArmProbeInterface;

/// ARM-specific errors
#[derive(Debug, thiserror::Error)]
#[error("An ARM specific error occured.")]
pub enum ArmError {
    /// The operation requires a specific architecture.
    #[error("The operation requires one of the following architectures: {0:?}")]
    ArchitectureRequired(&'static [&'static str]),
    /// A timeout occured during an operation
    #[error("Timeout occured during operation.")]
    Timeout,
    /// The address is too large for the 32 bit address space.
    #[error("Address is not in 32 bit address space.")]
    AddressOutOf32BitAddressSpace,
    /// The current target device is not an ARM device.
    #[error("Target device is not an ARM device.")]
    NoArmTarget,
    /// Error using a specific AP.
    #[error("Error using access port")]
    AccessPort {
        /// Address of the access port
        address: ApAddress,
        /// Source of the error.
        source: AccessPortError,
    },
    /// An error occured while using a debug port.
    #[error("Error using a debug port.")]
    DebugPort(#[from] DebugPortError),
    /// The core has to be halted for the operation, but was not.
    #[error("The core needs to be halted for this operation but was not.")]
    CoreNotHalted,
    /// Performing certain operations (e.g device unlock or Chip-Erase) can leave the device in a state
    /// that requires a probe re-attach to resolve.
    #[error("Probe and device internal state mismatch. A probe re-attach is required")]
    ReAttachRequired,
    /// An operation was not performed because the required permissions were not given.
    ///
    /// This can for example happen when the core is locked and needs to be erased to be unlocked.
    /// Then the correct permission needs to be given to automatically unlock the core to prevent accidental erases.
    #[error("An operation could not be performed because it lacked the permission to do so: {0}")]
    MissingPermissions(String),

    /// An error occured in the communication with an access port or debug port.
    #[error("An error occured in the communication with an access port or debug port.")]
    Dap(#[from] DapError),

    /// The debug probe encountered an error.
    #[error("The debug probe encountered an error.")]
    Probe(#[from] DebugProbeError),

    /// The given register address to perform an access on was not memory aligned.
    /// Make sure it is aligned to the size of the access (`address & access_size == 0`).
    #[error("Failed to access address 0x{address:08x} as it is not aligned to the requirement of {alignment} bytes for this platform and API call.")]
    MemoryNotAligned {
        /// The address of the register.
        address: u64,
        /// The required alignment in bytes (address increments).
        alignment: usize,
    },
    /// A region ouside of the AP address space was accessed.
    #[error("Out of bounds access")]
    OutOfBounds,
    /// The requested memory transfer width is not supported on the current core.
    #[error("{0} bit is not a supported memory transfer width on the current core")]
    UnsupportedTransferWidth(usize),

    /// The AP with the specified address does not exist.
    #[error("The AP with address {0:?} does not exist.")]
    ApDoesNotExist(ApAddress),

    /// The AP has the wrong type for the operation.
    WrongApType,

    /// It is not possible to create a breakpoint a the given address.
    #[error("Unable to create a breakpoint at address {0:#010X}. Hardware breakpoints are only supported at addresses < 0x2000'0000.")]
    UnsupportedBreakpointAddress(u32),

    /// ARMv8a specifc erorr occured.
    Armv8a(#[from] Armv8aError),

    /// ARMv7a specifc erorr occured.
    Armv7a(#[from] Armv7aError),

    /// Error occured in a debug sequence.
    DebugSequence(#[from] ArmDebugSequenceError),

    /// Tracing has not been configured.
    TracingUnconfigured,

    /// Error parsing a register.
    RegisterParse(#[from] RegisterParseError),

    /// Error reading ROM table.
    RomTable(#[source] RomTableError),

    /// Failed to erase chip
    ChipEraseFailed,
}

impl ArmError {
    /// Constructs [`ArmError::MemoryNotAligned`] from the address and the required alignment.
    pub fn from_access_port(err: AccessPortError, ap: impl AccessPort) -> Self {
        ArmError::AccessPort {
            address: ap.ap_address(),
            source: err,
        }
    }

    /// Constructs a [`ArmError::MemoryNotAligned`] from the address and the required alignment.
    pub fn alignment_error(address: u64, alignment: usize) -> Self {
        ArmError::MemoryNotAligned { address, alignment }
    }
}

impl From<RomTableError> for ArmError {
    fn from(value: RomTableError) -> Self {
        match value {
            RomTableError::Memory(err) => *err,
            other => ArmError::RomTable(other),
        }
    }
}

/// Check if the address is a valid 32 bit address. This functions
/// is ARM specific for ease of use, so that a specific error code can be returned.
pub fn valid_32bit_arm_address(address: u64) -> Result<u32, ArmError> {
    address
        .try_into()
        .map_err(|_| ArmError::AddressOutOf32BitAddressSpace)
}
