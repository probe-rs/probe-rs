use coresight::access_ports::generic_ap::GenericAP;
use coresight::access_ports::memory_ap::MemoryAP;
use coresight::ap_access::APAccess;
use coresight::access_ports::APRegister;
use coresight::ap_access::AccessPort;
use memory::ToMemoryReadSize;
use memory::adi_v5_memory_interface::ADIMemoryInterface;
use coresight::access_ports::AccessPortError;
use crate::protocol::WireProtocol;
use memory::MI;
use query_interface::{
    Object
};
use coresight::dap_access::DAPAccess;

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


pub struct MasterProbe {
    actual_probe: Box<DebugProbe>,
    current_apsel: u8,
    current_apbanksel: u8,
}

impl MasterProbe {
    pub fn from_specific_probe(p: Box<DebugProbe>) -> Self {
        MasterProbe {
            actual_probe: p,
            current_apbanksel: 0,
            current_apsel: 0,
        }
    }

    pub fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.actual_probe.target_reset()
    }
}

fn read_register_ap<AP, REGISTER>(probe: &mut MasterProbe, port: AP, _register: REGISTER) -> Option<REGISTER>
where
    AP: AccessPort,
    REGISTER: APRegister<AP>
{
    let link = &mut probe.actual_probe;
    use coresight::ap_access::AccessPort;
    // TODO: Make those next lines use the future typed DP interface.
    let cache_changed = if probe.current_apsel != port.get_port_number() {
        probe.current_apsel = port.get_port_number();
        true
    } else if probe.current_apbanksel != REGISTER::APBANKSEL {
        probe.current_apbanksel = REGISTER::APBANKSEL;
        true
    } else {
        false
    };
    if cache_changed {
        let select = (u32::from(probe.current_apsel) << 24) | (u32::from(probe.current_apbanksel) << 4);
        link.write_register(0xFFFF, 0x008, select)?;
    }
    //println!("{:?}, {:08X}", link.current_apsel, REGISTER::ADDRESS);
    let result = link.read_register(u16::from(probe.current_apsel), u16::from(REGISTER::ADDRESS))?;
    Some(REGISTER::from(result))
}

fn write_register_ap<AP, REGISTER>(probe: &mut MasterProbe, port: AP, register: REGISTER) -> Option<()>
where
    AP: AccessPort,
    REGISTER: APRegister<AP>
{
    let link = &mut probe.actual_probe;
    use coresight::ap_access::AccessPort;
    // TODO: Make those next lines use the future typed DP interface.
    let cache_changed = if probe.current_apsel != port.get_port_number() {
        probe.current_apsel = port.get_port_number();
        true
    } else if probe.current_apbanksel != REGISTER::APBANKSEL {
        probe.current_apbanksel = REGISTER::APBANKSEL;
        true
    } else {
        false
    };
    if cache_changed {
        let select = (u32::from(probe.current_apsel) << 24) | (u32::from(probe.current_apbanksel) << 4);
        link.write_register(0xFFFF, 0x008, select)?;
    }
    link.write_register(u16::from(probe.current_apsel), u16::from(REGISTER::ADDRESS), register.into())?;
    Some(())
}

impl<REGISTER> APAccess<MemoryAP, REGISTER> for MasterProbe
where
    REGISTER: APRegister<MemoryAP>
{
    type Error = DebugProbeError;

    fn read_register_ap(&mut self, port: MemoryAP, register: REGISTER) -> Result<REGISTER, Self::Error> {
        read_register_ap(self, port, register).ok_or(DebugProbeError::UnknownError)
    }
    
    fn write_register_ap(&mut self, port: MemoryAP, register: REGISTER) -> Result<(), Self::Error> {
        write_register_ap(self, port, register).ok_or(DebugProbeError::UnknownError)
    }
}

impl<REGISTER> APAccess<GenericAP, REGISTER> for MasterProbe
where
    REGISTER: APRegister<GenericAP>
{
    type Error = DebugProbeError;

    fn read_register_ap(&mut self, port: GenericAP, register: REGISTER) -> Result<REGISTER, Self::Error> {
        read_register_ap(self, port, register).ok_or(DebugProbeError::UnknownError)
    }
    
    fn write_register_ap(&mut self, port: GenericAP, register: REGISTER) -> Result<(), Self::Error> {
        write_register_ap(self, port, register).ok_or(DebugProbeError::UnknownError)
    }
}

impl MI for MasterProbe
{
    fn read<S: ToMemoryReadSize>(&mut self, address: u32) -> Result<S, AccessPortError> {
        ADIMemoryInterface::new(0).read(self, address)
    }

    fn read_block<S: ToMemoryReadSize>(
        &mut self,
        address: u32,
        data: &mut [S]
    ) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).read_block(self, address, data)
    }

    fn write<S: ToMemoryReadSize>(
        &mut self,
        addr: u32,
        data: S
    ) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).write(self, addr, data)
    }

    fn write_block<S: ToMemoryReadSize>(
        &mut self,
        addr: u32,
        data: &[S]
    ) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).write_block(self, addr, data)
    }
}


pub trait DebugProbe: DAPAccess {
    fn new_from_probe_info(info: DebugProbeInfo) -> Result<Box<Self>, DebugProbeError> where Self: Sized;
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

//query_interface::mopo!(DebugProbe);

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


/*
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
*/