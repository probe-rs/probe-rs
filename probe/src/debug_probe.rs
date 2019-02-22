use crate::protocol::WireProtocol;

pub trait DebugProbe {
    type Error;

    /// Reads back the version of the Probe.
    /// TODO: Most likely this is bogus to be kept in here, as the interface is tailored to the ST-Link.
    fn get_version(&mut self) -> Result<(u8, u8), Self::Error>;

    /// Get human readable name for the probe
    fn get_name(&self) -> &str;

    /// Enters debug mode
    fn attach(&mut self, protocol: WireProtocol) -> Result<(), Self::Error>;

    /// Leave debug mode
    fn detach(&mut self) -> Result<(), Self::Error>;

    /// Resets the target device.
    fn target_reset(&mut self) -> Result<(), Self::Error>;
}
