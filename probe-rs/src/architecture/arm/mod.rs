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

use self::ap::AccessPortError;
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
    /// Temporary Placeholder error, should be removed.
    #[error("Placeholder error")]
    Common(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The current target device is not an ARM device.
    #[error("Target device is not an ARM device.")]
    NoArmTarget,
    /// Error using a specific AP.
    #[error("Error using access port.")]
    AccessPort(#[from] AccessPortError),
    /// The core has to be halted for the operation, but was not.
    #[error("The core needs to be halted for this operation but was not.")]
    CoreNotHalted,
}

impl ArmError {
    /// Create a error based on an anyhow error.
    pub fn temporary(err: anyhow::Error) -> Self {
        ArmError::Common(err.into())
    }
}

impl From<DebugProbeError> for ArmError {
    fn from(value: DebugProbeError) -> Self {
        ArmError::Common(Box::new(value))
    }
}

impl From<DebugPortError> for ArmError {
    fn from(value: DebugPortError) -> Self {
        match value {
            DebugPortError::Arm(e) => *e,
            other => ArmError::Common(Box::new(other)),
        }
    }
}

impl From<RegisterParseError> for ArmError {
    fn from(value: RegisterParseError) -> Self {
        ArmError::Common(Box::new(value))
    }
}

impl From<RomTableError> for ArmError {
    fn from(value: RomTableError) -> Self {
        ArmError::Common(Box::new(value))
    }
}

impl From<DapError> for ArmError {
    fn from(value: DapError) -> Self {
        ArmError::Common(Box::new(value))
    }
}

impl From<ArmDebugSequenceError> for ArmError {
    fn from(value: ArmDebugSequenceError) -> Self {
        ArmError::Common(Box::new(value))
    }
}

/// Check if the address is a valid 32 bit address. This functions
/// is ARM specific for ease of use, so that a specific error code can be returned.
pub fn valid_32bit_arm_address(address: u64) -> Result<u32, ArmError> {
    address
        .try_into()
        .map_err(|_| ArmError::AddressOutOf32BitAddressSpace)
}
