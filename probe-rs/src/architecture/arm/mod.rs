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

pub use self::core::armv6m;
pub use self::core::armv7m;
pub use self::core::armv8m;
pub use self::core::Dump;

pub use communication_interface::ArmProbeInterface;
