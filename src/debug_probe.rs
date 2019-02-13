use crate::protocol::WireProtocol;

pub trait DebugProbe {
    type Error;

    fn open(&mut self) -> Result<(), Self::Error>;

    fn close(&mut self) -> Result<(), Self::Error>;

    fn get_version(&mut self) -> Result<(u8, u8), Self::Error>;

    /// Enters debug mode
    fn attach(&mut self, protocol: WireProtocol) -> Result<(), Self::Error>;

    /// Leave debug mode
    fn detach(&mut self) -> Result<(), Self::Error>;

    fn target_reset(&mut self) -> Result<(), Self::Error>;
}
