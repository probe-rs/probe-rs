use coresight::access_ports::generic_ap::GenericAP;
use coresight::access_ports::memory_ap::MemoryAP;
use coresight::access_ports::APRegister;

use coresight::ap_access::APAccess;
use coresight::ap_access::AccessPort;

use coresight::common::Register;

use log::debug;

use memory::ToMemoryReadSize;
use memory::adi_v5_memory_interface::ADIMemoryInterface;
use coresight::access_ports::AccessPortError;
use crate::protocol::WireProtocol;
use memory::MI;

use bitfield::bitfield;

use std::error::Error;
use std::fmt;

use crate::target::{
    m0::{
        Dhcsr,
        Dcrsr,
        Dcrdr,
        PC,
    },
    TargetRegister,
    CoreRegisterAddress,
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
}

impl Error for DebugProbeError {}

impl fmt::Display for DebugProbeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // TODO: Cleanup of Debug Probe Errors
        write!(f, "{:?}", self)
    }
}

impl From<AccessPortError> for DebugProbeError {
    fn from(value: AccessPortError) -> Self {
        DebugProbeError::UnknownError
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
}

impl MasterProbe {
    pub fn from_specific_probe(probe: Box<dyn DebugProbe>) -> Self {
        MasterProbe {
            actual_probe: probe,
            current_apbanksel: 0,
            current_apsel: 0,
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
        // TODO: Generic halt support

        let dhcsr_addr = 0xe000_edf0;
        let dhcsr_val: u32 = (0xa05f << 16) | (1 << 1) | (1 << 0);
        self.write(dhcsr_addr, dhcsr_val)?;
        
        // try to read the program counter
        let pc_value = self.read_core_reg(PC)?;

        // get pc
        Ok(CpuInformation {
            pc: pc_value,
        })
    }

    pub fn run(&mut self) -> Result<(), DebugProbeError> {
        let dhcsr_addr = 0xe000_edf0;
        let dhcsr_val: u32 = (0xa05f << 16) | (0 << 1) | (0 << 0);
        self.write(dhcsr_addr, dhcsr_val).map_err(Into::into)
    }

    /// Steps one instruction and then enters halted state again.
    /// Not tested!
    pub fn step(&mut self) -> Result<(), DebugProbeError> {
        let mut value = Dhcsr(0);
        // Leave halted state.
        // Step one instruction.
        value.set_C_STEP(true);
        value.set_C_HALT(false);
        self.write::<u32>(Dhcsr::ADDRESS, value.into())?;

        self.wait_for_core_halted()
    }

    pub fn read_core_reg(&mut self, addr: CoreRegisterAddress) -> Result<u32, DebugProbeError> {
        // write the dcrsr value to select the register we want to read,
        // in this case, the dcrsr register
        let mut dcrsr_val = Dcrsr(0);
        dcrsr_val.set_REGWnR(false);    // perform a read
        dcrsr_val.set_regsel(0b01111);  // read the debug return address (i.e. the next executed instruction)

        self.write::<u32>(Dcrsr::ADDRESS, dcrsr_val.into())?;

        self.wait_for_core_register_transfer()?;

        self.read(Dcrdr::ADDRESS).map_err(From::from)
    }

    pub fn write_core_reg(&mut self, addr: CoreRegisterAddress, value: u32) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::UnknownError)
    }

    fn wait_for_core_halted(&mut self) -> Result<(), DebugProbeError> {
        // Wait until halted state is active again.
        for _ in 0..100 {
            let dhcsr_val = Dhcsr(self.read(Dhcsr::ADDRESS)?);

            if dhcsr_val.S_HALT() {
                break;
            }
        }
        Err(DebugProbeError::UnknownError)
    }

    fn wait_for_core_register_transfer(&mut self) -> Result<(), DebugProbeError> {
        // now we have to poll the dhcsr register, until the dhcsr.s_regrdy bit is set
        // (see C1-292, cortex m0 arm)
        for _ in 0..100 {
            let dhcsr_val = Dhcsr(self.read(Dhcsr::ADDRESS)?);

            if dhcsr_val.S_REGRDY() {
                break;
            }
        }
        Err(DebugProbeError::UnknownError)
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