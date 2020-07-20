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

#[derive(Debug, Copy, Clone)]
pub enum SwoMode {
    UART,
    Manchester,
}

pub struct SwoConfig {
    /// SWO mode: either UART or Manchester.
    pub mode: SwoMode,

    /// Baud rate of SWO, in Hz.
    ///
    /// This value is used to configure what baud rate the target
    /// generates and to configure what baud rate the probe receives,
    /// so must be a baud rate supported by both target and probe.
    pub baud: u32,

    /// Clock input to TPIU in Hz. This is often the system clock (HCLK/SYSCLK etc).
    pub tpiu_clk: u32,
}

pub trait SwoAccess {
    /// Configure a SwoAccess interface for reading SWO data.
    fn enable_swo(&mut self, config: &SwoConfig) -> Result<(), Error>;

    /// Disable SWO reading on this SwoAccess interface.
    fn disable_swo(&mut self) -> Result<(), Error>;

    /// Read any available SWO data without waiting.
    ///
    /// Returns a Vec<u8> of received SWO bytes since the last `read_swo()` call.
    /// If no data was available, returns an empty Vec.
    fn read_swo(&mut self) -> Result<Vec<u8>, Error> {
        self.read_swo_timeout(std::time::Duration::from_millis(10))
    }

    /// Read SWO data for up to `timeout` duration.
    ///
    /// If no data is received before the timeout, returns an empty Vec.
    /// May return earlier than `timeout` if the receive buffer fills up.
    fn read_swo_timeout(&mut self, timeout: std::time::Duration) -> Result<Vec<u8>, Error>;
}
