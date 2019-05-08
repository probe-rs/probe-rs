use crate::protocol::WireProtocol;
use memory::MI;
use query_interface::{
    Object
};

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
    ProbeCouldNotBeCreated,
}


pub trait DebugProbe: Object {
    fn new_from_probe_info(info: DebugProbeInfo) -> Result<Probe, DebugProbeError> where Self: Sized;
    /// Reads back the version of the Probe.
    /// TODO: Most likely this is bogus to be kept in here, as the interface is tailored to the ST-Link.
    fn get_version(&mut self) -> Result<(u8, u8), DebugProbeError>;

    /// Get human readable name for the probe
    fn get_name(&self) -> &str;

    /// Enters debug mode
    fn attach(&mut self, protocol: Option<WireProtocol>) -> Result<WireProtocol, DebugProbeError>;

    /// Leave debug mode
    fn detach(&mut self) -> Result<(), DebugProbeError>;

    /// Resets the target device.
    fn target_reset(&mut self) -> Result<(), DebugProbeError>;
}

query_interface::mopo!(DebugProbe);

#[macro_export]
macro_rules! register_debug_probe {
    ($probe:ty: $($interfaces:ty),*) => {
        pub use $crate::query_interface::*;
        interfaces!($probe: $($interfaces),*);
    }
}

#[derive(Debug)]
pub enum DebugProbeType {
    DAPLink,
    STLink,
}

pub struct DebugProbeInfo {
    pub identifier: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
    pub probe_type: DebugProbeType,
}

impl std::fmt::Debug for DebugProbeInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f, "{} (VID: {}, PID: {}, {}{:?})",
            self.identifier,
            self.vendor_id,
            self.product_id,
            self.serial_number.clone().map_or("".to_owned(), |v| format!("Serial: {},", v)),
            self.probe_type
        )
    }
}

impl DebugProbeInfo {
    pub fn new<S: Into<String>>(
        identifier: S,
        vendor_id: u16,
        product_id: u16,
        serial_number: Option<String>,
        probe_type: DebugProbeType
    ) -> Self {
        Self {
            identifier: identifier.into(),
            vendor_id,
            product_id,
            serial_number,
            probe_type,
        }
    }
}

pub struct Probe {
    pub inner: Box<dyn Object>,
}

impl Probe {
    pub fn new<P: 'static + DebugProbe>(probe: P) -> Self {
        Self {
            inner: Box::new(probe)
        }
    }
    
    pub fn get_interface<T: 'static + ?Sized>(&self) -> Option<&T> {
        self.inner.query_ref::<T>()
    }
    
    pub fn get_interface_mut<T: 'static + ?Sized>(&mut self) -> Option<&mut T> {
        self.inner.query_mut::<T>()
    }
}