use coresight::{
    access_ports::{
        generic_ap::GenericAP,
        memory_ap::MemoryAP,
        APRegister,
        AccessPortError
    },
    ap_access::{
        APAccess,
        AccessPort,
    },
    common::Register,
};

use log::debug;

use memory::ToMemoryReadSize;
use memory::adi_v5_memory_interface::ADIMemoryInterface;
use crate::protocol::WireProtocol;
use memory::MI;
use std::error::Error;
use std::fmt;

use crate::target::{
    CoreRegisterAddress,
    Target,
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
    TargetPowerUpFailed,
    Timeout,
    AccessPortError(AccessPortError)
}

impl Error for DebugProbeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            DebugProbeError::AccessPortError(ref e) => Some(e),
            _ => None,
        }
    }
}

impl fmt::Display for DebugProbeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // TODO: Cleanup of Debug Probe Errors
        write!(f, "{:?}", self)
    }
}

impl From<AccessPortError> for DebugProbeError {
    fn from(e: AccessPortError) -> Self {
        DebugProbeError::AccessPortError(e)
    }
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
    actual_probe: Box<dyn DebugProbe>,
    current_apsel: u8,
    current_apbanksel: u8,
    target: Option<Target>,
}

impl MasterProbe {
    pub fn from_specific_probe(probe: Box<dyn DebugProbe>, target: Option<Target>) -> Self {
        MasterProbe {
            actual_probe: probe,
            current_apbanksel: 0,
            current_apsel: 0,
            target,
        }
    }

    pub fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.actual_probe.target_reset()
    }

    fn select_ap_and_ap_bank(&mut self, port: u8, ap_bank: u8) -> Result<(), DebugProbeError> {
        let mut cache_changed = if self.current_apsel != port {
            self.current_apsel = port;
            true
        } else {
            false
        };
        
        if self.current_apbanksel != ap_bank {
            self.current_apbanksel = ap_bank;
            cache_changed = true;
        }

        if cache_changed {
            use coresight::debug_port::Select;

            let mut select = Select(0);

            debug!("Changing AP to {}, AP_BANK_SEL to {}", self.current_apsel, self.current_apbanksel);

            select.set_ap_sel(self.current_apsel);
            select.set_ap_bank_sel(self.current_apbanksel);

            self.actual_probe.write_register(Port::DebugPort, u16::from(Select::ADDRESS), select.into())?;
        }

        Ok(())
    }

    fn write_register_ap<AP, REGISTER>(&mut self, port: AP, register: REGISTER) -> Result<(), DebugProbeError>
    where
        AP: AccessPort,
        REGISTER: APRegister<AP>
    {
        let register_value = register.into();

        debug!("Writing register {}, value=0x{:08X}", REGISTER::NAME, register_value);

        self.select_ap_and_ap_bank(port.get_port_number(), REGISTER::APBANKSEL)?;

        let link = &mut self.actual_probe;
        link.write_register(Port::AccessPort(u16::from(self.current_apsel)), u16::from(REGISTER::ADDRESS), register_value)?;
        Ok(())
    }

    fn read_register_ap<AP, REGISTER>(&mut self, port: AP, _register: REGISTER) -> Result<REGISTER, DebugProbeError>
    where
        AP: AccessPort,
        REGISTER: APRegister<AP>
    {
        debug!("Reading register {}", REGISTER::NAME);
        self.select_ap_and_ap_bank(port.get_port_number(), REGISTER::APBANKSEL)?;

        let link = &mut self.actual_probe;
        //println!("{:?}, {:08X}", link.current_apsel, REGISTER::ADDRESS);
        let result = link.read_register(Port::AccessPort(u16::from(self.current_apsel)), u16::from(REGISTER::ADDRESS))?;

        debug!("Read register    {}, value=0x{:08x}", REGISTER::NAME, result);

        Ok(REGISTER::from(result))
    }

    pub fn halt(&mut self) -> Result<CpuInformation, DebugProbeError> {
        self.target.map(|t| (t.halt)(self)).unwrap()
    }

    pub fn run(&mut self) -> Result<(), DebugProbeError> {
        self.target.map(|t| (t.run)(self)).unwrap()
    }

    /// Steps one instruction and then enters halted state again.
    /// Not tested!
    pub fn step(&mut self) -> Result<CpuInformation, DebugProbeError> {
        self.target.map(|t| (t.step)(self)).unwrap()
    }

    pub fn read_core_reg(&mut self, addr: CoreRegisterAddress) -> Result<u32, DebugProbeError> {
        self.target.map(|t| (t.read_core_reg)(self, addr)).unwrap()
    }

    pub fn write_core_reg(&mut self, addr: CoreRegisterAddress, value: u32) -> Result<(), DebugProbeError> {
        self.target.map(|t| (t.write_core_reg)(self, addr, value)).unwrap()
    }
}

pub struct CpuInformation {
    pub pc: u32,
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
    fn read32(&mut self, address: u32) -> Result<u32, AccessPortError> {
        ADIMemoryInterface::new(0).read32(self, address)
    }

    fn read8(&mut self, address: u32) -> Result<u8, AccessPortError> {
        ADIMemoryInterface::new(0).read8(self, address)
    }

    fn read_block32(
        &mut self,
        address: u32,
        data: &mut [u32]
    ) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).read_block32(self, address, data)
    }

    fn read_block8(
        &mut self,
        address: u32,
        data: &mut [u8]
    ) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).read_block8(self, address, data)
    }

    fn write32(
        &mut self,
        addr: u32,
        data: u32
    ) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).write32(self, addr, data)
    }

    fn write_block32(
        &mut self,
        addr: u32,
        data: &[u32]
    ) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).write_block32(self, addr, data)
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