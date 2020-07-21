pub mod ap;
pub(crate) mod communication_interface;
pub mod component;
pub(crate) mod core;
pub mod dp;
pub mod memory;
pub mod swo;

pub use communication_interface::{
    ArmChipInfo, ArmCommunicationInterface, ArmCommunicationInterfaceState, DAPAccess, DapError,
};
pub use communication_interface::{PortType, Register};
pub use swo::{SwoConfig, SwoAccess, SwoMode};

pub use self::core::m0;
pub use self::core::m33;
pub use self::core::m4;
pub use self::core::CortexDump;
