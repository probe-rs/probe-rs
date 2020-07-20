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

pub use self::core::m0;
pub use self::core::m33;
pub use self::core::m4;
pub use self::core::CortexDump;
use crate::Error;

pub trait SwoAccess {
    fn read_swo(&mut self) -> Result<Vec<u8>, Error> {
        self.read_swo_timeout(std::time::Duration::from_millis(10))
    }

    fn read_swo_timeout(&mut self, timeout: std::time::Duration) -> Result<Vec<u8>, Error>;
}
