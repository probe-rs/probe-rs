pub mod ap;
pub(crate) mod communication_interface;
pub mod component;
pub(crate) mod core;
pub mod dp;
pub mod memory;
pub mod swo;

pub use communication_interface::{
    ApInformation, ArmChipInfo, ArmCommunicationInterface, DAPAccess, DapError,
};
pub use communication_interface::{PortType, Register};
pub use swo::{SwoAccess, SwoConfig, SwoMode};

pub use self::core::m0;
pub use self::core::m33;
pub use self::core::m4;
pub use self::core::CortexDump;
