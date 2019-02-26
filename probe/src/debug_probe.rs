use crate::protocol::WireProtocol;
use memory::MI;

#[derive(Debug)]
pub enum DebugProbeError {
    USBError,
    JTAGNotSupportedOnProbe,
    ProbeFirmwareOutdated,
    VoltageDivisionByZero,
    UnknownMode,
    JTagDoesNotSupportMultipleAP,
    UnknownError,
    TransferFault(u32, u16),
    DataAlignmentError,
    Access16BitNotSupported,
    BlanksNotAllowedOnDPRegister,
    RegisterAddressMustBe16Bit,
    NotEnoughBytesRead,
    EndpointNotFound,
    RentalInitError,
}


pub trait DebugProbe: MI {
    type Error;

    /// Reads back the version of the Probe.
    /// TODO: Most likely this is bogus to be kept in here, as the interface is tailored to the ST-Link.
    fn get_version(&mut self) -> Result<(u8, u8), <Self as DebugProbe>::Error>;

    /// Get human readable name for the probe
    fn get_name(&self) -> &str;

    /// Enters debug mode
    fn attach(&mut self, protocol: WireProtocol) -> Result<(), <Self as DebugProbe>::Error>;

    /// Leave debug mode
    fn detach(&mut self) -> Result<(), <Self as DebugProbe>::Error>;

    /// Resets the target device.
    fn target_reset(&mut self) -> Result<(), <Self as DebugProbe>::Error>;
}
