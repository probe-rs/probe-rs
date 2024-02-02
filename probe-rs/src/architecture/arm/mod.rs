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

pub use self::core::{armv6m, armv7a, armv7m, armv8a, armv8m, Dump};
use self::{
    ap::{AccessPort, AccessPortError},
    communication_interface::RegisterParseError,
    dp::DebugPortError,
    memory::romtable::RomTableError,
    sequences::ArmDebugSequenceError,
    {armv7a::Armv7aError, armv8a::Armv8aError},
};
use crate::probe::DebugProbeError;
pub use communication_interface::{
    ApInformation, ArmChipInfo, ArmCommunicationInterface, ArmProbeInterface, DapError,
    MemoryApInformation, Register,
};
pub use swo::{SwoAccess, SwoConfig, SwoMode, SwoReader};
pub use traits::*;

/// ARM-specific errors
#[derive(Debug, displaydoc::Display, thiserror::Error)]
#[ignore_extra_doc_attributes]
pub enum ArmError {
    /// The operation requires one of the following architectures: {0:?}
    ArchitectureRequired(&'static [&'static str]),
    /// A timeout occurred during an operation
    Timeout,
    /// The address is too large for the 32 bit address space.
    AddressOutOf32BitAddressSpace,
    /// The current target device is not an ARM device.
    NoArmTarget,
    /// Error using access port {address:?}.
    AccessPort {
        /// Address of the access port
        address: ApAddress,
        /// Source of the error.
        source: AccessPortError,
    },
    /// An error occurred while using a debug port.
    DebugPort(#[from] DebugPortError),
    /// The core needs to be halted for this operation but was not.
    CoreNotHalted,
    /// Probe and device internal state mismatch. A probe re-attach is required
    ReAttachRequired,
    /// An operation could not be performed because it lacked the permission to do so: {0}
    ///
    /// This can for example happen when the core is locked and needs to be erased to be unlocked.
    /// Then the correct permission needs to be given to automatically unlock the core to prevent accidental erases.
    MissingPermissions(String),

    /// An error occurred in the communication with an access port or debug port.
    Dap(#[from] DapError),

    /// The debug probe encountered an error.
    Probe(#[from] DebugProbeError),

    /// Failed to access address 0x{address:08x} as it is not aligned to the requirement of {alignment} bytes for this platform and API call.
    ///
    /// The given register address to perform an access on was not memory aligned.
    /// Make sure it is aligned to the size of the access (`address & access_size == 0`).
    MemoryNotAligned {
        /// The address of the register.
        address: u64,
        /// The required alignment in bytes (address increments).
        alignment: usize,
    },
    /// A region outside of the AP address space was accessed.
    OutOfBounds,

    /// {0} bit is not a supported memory transfer width on the current core.
    UnsupportedTransferWidth(usize),

    /// The AP with address {0:?} does not exist.
    ApDoesNotExist(ApAddress),

    /// The AP has the wrong type for the operation.
    WrongApType,

    /// Unable to create a breakpoint at address {0:#010X}. Hardware breakpoints are only supported at addresses < 0x2000'0000.
    UnsupportedBreakpointAddress(u32),

    /// An ARMv8a specific error occurred.
    Armv8a(#[from] Armv8aError),

    /// An ARMv7a specific error occurred.
    Armv7a(#[from] Armv7aError),

    /// Error occurred in a debug sequence.
    DebugSequence(#[from] ArmDebugSequenceError),

    /// Tracing has not been configured.
    TracingUnconfigured,

    /// Error parsing a register.
    RegisterParse(#[from] RegisterParseError),

    /// Error reading ROM table.
    RomTable(#[source] RomTableError),

    /// Failed to erase chip
    ChipEraseFailed,

    /// The operation requires the following extension(s): {0:?}
    ExtensionRequired(&'static [&'static str]),

    /// Any other error occurred.
    Other(#[from] anyhow::Error),
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
