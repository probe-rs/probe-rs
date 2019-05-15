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

#[derive(Debug, PartialEq)]
pub enum Port {
    DebugPort,
    AccessPort(u16)
}

pub trait DAPAccess {
    /// Reads the DAP register on the specified port and address
    fn read_register(&mut self, port: Port, addr: u16) -> Result<u32, DebugProbeError>;

    /// Writes a value to the DAP register on the specified port and address
    fn write_register(&mut self, port: Port, addr: u16, value: u32) -> Result<(), DebugProbeError>;
}


pub struct MasterProbe {
    actual_probe: Box<DebugProbe>,
    current_apsel: u8,
    current_apbanksel: u8,
}

impl MasterProbe {
    pub fn from_specific_probe(probe: Box<DebugProbe>) -> Self {
        MasterProbe {
            actual_probe: probe,
            current_apbanksel: 0,
            current_apsel: 0,
        }
    }

    pub fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.actual_probe.target_reset()
    }

    fn write_register_ap<AP, REGISTER>(&mut self, port: AP, register: REGISTER) -> Result<(), DebugProbeError>
    where
        AP: AccessPort,
        REGISTER: APRegister<AP>
    {
        let link = &mut self.actual_probe;
        // TODO: Make those next lines use the future typed DP interface.
        let cache_changed = if self.current_apsel != port.get_port_number() {
            self.current_apsel = port.get_port_number();
            true
        } else if self.current_apbanksel != REGISTER::APBANKSEL {
            self.current_apbanksel = REGISTER::APBANKSEL;
            true
        } else {
            false
        };
        if cache_changed {
            let select = (u32::from(self.current_apsel) << 24) | (u32::from(self.current_apbanksel) << 4);
            link.write_register(Port::DebugPort, 0x008, select)?;
        }
        link.write_register(Port::AccessPort(u16::from(self.current_apsel)), u16::from(REGISTER::ADDRESS), register.into())?;
        Ok(())
    }

    fn read_register_ap<AP, REGISTER>(&mut self, port: AP, _register: REGISTER) -> Result<REGISTER, DebugProbeError>
    where
        AP: AccessPort,
        REGISTER: APRegister<AP>
    {
        let link = &mut self.actual_probe;
        // TODO: Make those next lines use the future typed DP interface.
        let cache_changed = if self.current_apsel != port.get_port_number() {
            self.current_apsel = port.get_port_number();
            true
        } else if self.current_apbanksel != REGISTER::APBANKSEL {
            self.current_apbanksel = REGISTER::APBANKSEL;
            true
        } else {
            false
        };
        if cache_changed {
            let select = (u32::from(self.current_apsel) << 24) | (u32::from(self.current_apbanksel) << 4);
            link.write_register(Port::DebugPort, 0x008, select)?;
        }
        //println!("{:?}, {:08X}", link.current_apsel, REGISTER::ADDRESS);
        let result = link.read_register(Port::AccessPort(u16::from(self.current_apsel)), u16::from(REGISTER::ADDRESS))?;
        Ok(REGISTER::from(result))
    }
}



impl<REGISTER> APAccess<MemoryAP, REGISTER> for MasterProbe
where
    REGISTER: APRegister<MemoryAP>
{
    type Error = DebugProbeError;

    fn read_register_ap(&mut self, port: MemoryAP, register: REGISTER) -> Result<REGISTER, Self::Error> {
        self.read_register_ap(port, register)
    }
    
    fn write_register_ap(&mut self, port: MemoryAP, register: REGISTER) -> Result<(), Self::Error> {
        self.write_register_ap(port, register)
    }
}

impl<REGISTER> APAccess<GenericAP, REGISTER> for MasterProbe
where
    REGISTER: APRegister<GenericAP>
{
    type Error = DebugProbeError;

    fn read_register_ap(&mut self, port: GenericAP, register: REGISTER) -> Result<REGISTER, Self::Error> {
        self.read_register_ap(port, register)
    }
    
    fn write_register_ap(&mut self, port: GenericAP, register: REGISTER) -> Result<(), Self::Error> {
        self.write_register_ap(port, register)
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

    /// Get human readable name for the probe
    fn get_name(&self) -> &str;

    /// Enters debug mode
    fn attach(&mut self, protocol: Option<WireProtocol>) -> Result<WireProtocol, DebugProbeError>;

    /// Leave debug mode
    fn detach(&mut self) -> Result<(), DebugProbeError>;

    /// Resets the target device.
    fn target_reset(&mut self) -> Result<(), DebugProbeError>;
}


impl std::ops::Deref for MasterProbe {
    type Target = dyn DebugProbe;

    fn deref(&self) -> &Self::Target {
        &*self.actual_probe
    }
}


impl std::ops::DerefMut for MasterProbe {
    fn deref_mut(&mut self) -> &mut <Self as std::ops::Deref>::Target {
        &mut *self.actual_probe
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