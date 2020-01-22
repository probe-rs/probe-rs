pub mod daplink;
pub mod stlink;

use crate::coresight::{
    access_ports::{
        custom_ap::{CtrlAP, ERASEALL, ERASEALLSTATUS, RESET},
        generic_ap::{APClass, APType, GenericAP, IDR},
        memory_ap::MemoryAP,
        APRegister, AccessPortError,
    },
    ap_access::{get_ap_by_idr, APAccess, AccessPort},
    common::Register,
    memory::{adi_v5_memory_interface::ADIMemoryInterface, MI},
};

use log::debug;

use colored::*;
use std::error::Error;
use std::time::Instant;

use thiserror::Error;

#[derive(Copy, Clone, Debug)]
pub enum WireProtocol {
    Swd,
    Jtag,
}

const UNLOCK_TIMEOUT: u64 = 15;
const CTRL_AP_IDR: IDR = IDR {
    REVISION: 0,
    DESIGNER: 0x0144,
    CLASS: APClass::Undefined,
    _RES0: 0,
    VARIANT: 0,
    TYPE: APType::JTAG_COM_AP,
};

#[derive(Error, Debug)]
pub enum DebugProbeError {
    #[error("USB Communication Error")]
    USBError(#[source] Option<Box<dyn Error + Send + Sync>>),
    #[error("JTAG not supported on probe")]
    JTAGNotSupportedOnProbe,
    #[error("The firmware on the probe is outdated")]
    ProbeFirmwareOutdated,
    #[error("Error specific to a probe type occured")]
    ProbeSpecificError(#[source] Box<dyn Error + Send + Sync>),
    // TODO: Unknown errors are not very useful, this should be removed.
    #[error("An unknown error occured.")]
    UnknownError,
    #[error("Probe could not be created.")]
    ProbeCouldNotBeCreated,
    // TODO: This is core specific, so should probably be moved there.
    #[error("Operation timed out.")]
    Timeout,
    #[error("Communication with access port failed: {0:?}")]
    AccessPortError(#[from] AccessPortError),
}

impl From<stlink::StlinkError> for DebugProbeError {
    fn from(e: stlink::StlinkError) -> Self {
        DebugProbeError::ProbeSpecificError(Box::new(e))
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Port {
    DebugPort,
    AccessPort(u16),
}

pub trait DAPAccess {
    /// Reads the DAP register on the specified port and address
    fn read_register(&mut self, port: Port, addr: u16) -> Result<u32, DebugProbeError>;

    /// Read multiple values from the same DAP register.
    ///
    /// If possible, this uses optimized read functions, otherwise it
    /// falls back to the `read_register` function.
    fn read_block(
        &mut self,
        port: Port,
        addr: u16,
        values: &mut [u32],
    ) -> Result<(), DebugProbeError> {
        for val in values {
            *val = self.read_register(port, addr)?;
        }

        Ok(())
    }

    /// Writes a value to the DAP register on the specified port and address
    fn write_register(&mut self, port: Port, addr: u16, value: u32) -> Result<(), DebugProbeError>;

    /// Write multiple values to the same DAP register.
    ///
    /// If possible, this uses optimized write functions, otherwise it
    /// falls back to the `write_register` function.
    fn write_block(
        &mut self,
        port: Port,
        addr: u16,
        values: &[u32],
    ) -> Result<(), DebugProbeError> {
        for val in values {
            self.write_register(port, addr, *val)?;
        }

        Ok(())
    }
}

/// The MasterProbe struct is a generic wrapper over the different
/// probes supported.
///
/// # Examples
///
/// ## Open the first probe found
///
/// The `list_all` and `from_probe_info` functions can be used
/// to create a new `MasterProbe`::
///
/// ```no_run
/// use probe_rs::probe::MasterProbe;
///
/// let probe_list = MasterProbe::list_all();
/// let probe = MasterProbe::from_probe_info(&probe_list[0]);
/// ```

pub struct MasterProbe {
    actual_probe: Box<dyn DebugProbe>,
    current_apsel: u8,
    current_apbanksel: u8,
}

impl MasterProbe {
    /// Get a list of all debug probes found.
    /// This can be used to select the debug probe which
    /// should be used.
    pub fn list_all() -> Vec<DebugProbeInfo> {
        let mut list = daplink::tools::list_daplink_devices();
        list.extend(stlink::tools::list_stlink_devices());

        list
    }

    /// Create a `MasterProbe` from `DebugProbeInfo`. Use the
    /// `MasterProbe::list_all()` function to get the information
    /// about all probes available.
    pub fn from_probe_info(info: &DebugProbeInfo) -> Result<Self, DebugProbeError> {
        let probe = match info.probe_type {
            DebugProbeType::DAPLink => {
                let mut dap_link = daplink::DAPLink::new_from_probe_info(info)?;

                dap_link.attach(Some(WireProtocol::Swd))?;

                MasterProbe::from_specific_probe(dap_link)
            }
            DebugProbeType::STLink => {
                let mut link = stlink::STLink::new_from_probe_info(info)?;

                link.attach(Some(WireProtocol::Swd))?;

                MasterProbe::from_specific_probe(link)
            }
        };

        Ok(probe)
    }

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
            use crate::coresight::debug_port::Select;

            let mut select = Select(0);

            debug!(
                "Changing AP to {}, AP_BANK_SEL to {}",
                self.current_apsel, self.current_apbanksel
            );

            select.set_ap_sel(self.current_apsel);
            select.set_ap_bank_sel(self.current_apbanksel);

            self.actual_probe.write_register(
                Port::DebugPort,
                u16::from(Select::ADDRESS),
                select.into(),
            )?;
        }

        Ok(())
    }

    fn write_ap_register<AP, REGISTER>(
        &mut self,
        port: AP,
        register: REGISTER,
    ) -> Result<(), DebugProbeError>
    where
        AP: AccessPort,
        REGISTER: APRegister<AP>,
    {
        let register_value = register.into();

        debug!(
            "Writing register {}, value=0x{:08X}",
            REGISTER::NAME,
            register_value
        );

        self.select_ap_and_ap_bank(port.get_port_number(), REGISTER::APBANKSEL)?;

        let link = &mut self.actual_probe;
        link.write_register(
            Port::AccessPort(u16::from(self.current_apsel)),
            u16::from(REGISTER::ADDRESS),
            register_value,
        )?;
        Ok(())
    }

    fn write_ap_register_repeated<AP, REGISTER>(
        &mut self,
        port: AP,
        _register: REGISTER,
        values: &[u32],
    ) -> Result<(), DebugProbeError>
    where
        AP: AccessPort,
        REGISTER: APRegister<AP>,
    {
        debug!(
            "Writing register {}, block with len={} words",
            REGISTER::NAME,
            values.len(),
        );

        self.select_ap_and_ap_bank(port.get_port_number(), REGISTER::APBANKSEL)?;

        let link = &mut self.actual_probe;
        link.write_block(
            Port::AccessPort(u16::from(self.current_apsel)),
            u16::from(REGISTER::ADDRESS),
            values,
        )?;
        Ok(())
    }

    fn read_ap_register<AP, REGISTER>(
        &mut self,
        port: AP,
        _register: REGISTER,
    ) -> Result<REGISTER, DebugProbeError>
    where
        AP: AccessPort,
        REGISTER: APRegister<AP>,
    {
        debug!("Reading register {}", REGISTER::NAME);
        self.select_ap_and_ap_bank(port.get_port_number(), REGISTER::APBANKSEL)?;

        let link = &mut self.actual_probe;
        //log::debug!("{:?}, {:08X}", link.current_apsel, REGISTER::ADDRESS);
        let result = link.read_register(
            Port::AccessPort(u16::from(self.current_apsel)),
            u16::from(REGISTER::ADDRESS),
        )?;

        debug!(
            "Read register    {}, value=0x{:08x}",
            REGISTER::NAME,
            result
        );

        Ok(REGISTER::from(result))
    }

    fn read_ap_register_repeated<AP, REGISTER>(
        &mut self,
        port: AP,
        _register: REGISTER,
        values: &mut [u32],
    ) -> Result<(), DebugProbeError>
    where
        AP: AccessPort,
        REGISTER: APRegister<AP>,
    {
        debug!(
            "Reading register {}, block with len={} words",
            REGISTER::NAME,
            values.len(),
        );

        self.select_ap_and_ap_bank(port.get_port_number(), REGISTER::APBANKSEL)?;

        let link = &mut self.actual_probe;
        link.read_block(
            Port::AccessPort(u16::from(self.current_apsel)),
            u16::from(REGISTER::ADDRESS),
            values,
        )?;
        Ok(())
    }

    pub fn read_register_dp(&mut self, offset: u16) -> Result<u32, DebugProbeError> {
        self.actual_probe.read_register(Port::DebugPort, offset)
    }

    pub fn write_register_dp(&mut self, offset: u16, val: u32) -> Result<(), DebugProbeError> {
        self.actual_probe
            .write_register(Port::DebugPort, offset, val)
    }

    /// Tries to mass erase a locked nRF52 chip, this process may timeout, if it does, the chip
    /// might be unlocked or not, it is advised to try again if flashing fails
    pub fn nrf_recover(&mut self) -> Result<(), DebugProbeError> {
        let ctrl_port = match get_ap_by_idr(self, |idr| idr == CTRL_AP_IDR) {
            Some(port) => CtrlAP::from(port),
            None => {
                return Err(DebugProbeError::AccessPortError(
                    AccessPortError::CtrlAPNotFound,
                ));
            }
        };
        log::info!("Starting mass erase...");
        let mut erase_reg = ERASEALL::from(1);
        let status_reg = ERASEALLSTATUS::from(0);
        let mut reset_reg = RESET::from(1);

        // Reset first
        self.write_ap_register(ctrl_port, reset_reg)?;
        reset_reg.RESET = false;
        self.write_ap_register(ctrl_port, reset_reg)?;

        self.write_ap_register(ctrl_port, erase_reg)?;

        // Prepare timeout
        let now = Instant::now();
        let status = self.read_ap_register(ctrl_port, status_reg)?;
        log::info!("Erase status: {:?}", status.ERASEALLSTATUS);
        let timeout = loop {
            let status = self.read_ap_register(ctrl_port, status_reg)?;
            if !status.ERASEALLSTATUS {
                break false;
            }
            if now.elapsed().as_secs() >= UNLOCK_TIMEOUT {
                break true;
            }
        };
        reset_reg.RESET = true;
        self.write_ap_register(ctrl_port, reset_reg)?;
        reset_reg.RESET = false;
        self.write_ap_register(ctrl_port, reset_reg)?;
        erase_reg.ERASEALL = false;
        self.write_ap_register(ctrl_port, erase_reg)?;
        if timeout {
            log::error!(
                "    {} Mass erase process timeout, the chip might still be locked.",
                "Error".red().bold()
            );
        } else {
            log::info!("Mass erase completed, chip unlocked");
        }
        Ok(())
    }
}

impl<REGISTER> APAccess<MemoryAP, REGISTER> for MasterProbe
where
    REGISTER: APRegister<MemoryAP>,
{
    type Error = DebugProbeError;

    fn read_ap_register(
        &mut self,
        port: MemoryAP,
        register: REGISTER,
    ) -> Result<REGISTER, Self::Error> {
        self.read_ap_register(port, register)
    }

    fn write_ap_register(&mut self, port: MemoryAP, register: REGISTER) -> Result<(), Self::Error> {
        self.write_ap_register(port, register)
    }

    fn write_ap_register_repeated(
        &mut self,
        port: MemoryAP,
        register: REGISTER,
        values: &[u32],
    ) -> Result<(), Self::Error> {
        self.write_ap_register_repeated(port, register, values)
    }

    fn read_ap_register_repeated(
        &mut self,
        port: MemoryAP,
        register: REGISTER,
        values: &mut [u32],
    ) -> Result<(), Self::Error> {
        self.read_ap_register_repeated(port, register, values)
    }
}

impl<REGISTER> APAccess<GenericAP, REGISTER> for MasterProbe
where
    REGISTER: APRegister<GenericAP>,
{
    type Error = DebugProbeError;

    fn read_ap_register(
        &mut self,
        port: GenericAP,
        register: REGISTER,
    ) -> Result<REGISTER, Self::Error> {
        self.read_ap_register(port, register)
    }

    fn write_ap_register(
        &mut self,
        port: GenericAP,
        register: REGISTER,
    ) -> Result<(), Self::Error> {
        self.write_ap_register(port, register)
    }

    fn write_ap_register_repeated(
        &mut self,
        port: GenericAP,
        register: REGISTER,
        values: &[u32],
    ) -> Result<(), Self::Error> {
        self.write_ap_register_repeated(port, register, values)
    }

    fn read_ap_register_repeated(
        &mut self,
        port: GenericAP,
        register: REGISTER,
        values: &mut [u32],
    ) -> Result<(), Self::Error> {
        self.read_ap_register_repeated(port, register, values)
    }
}

impl MI for MasterProbe {
    fn read32(&mut self, address: u32) -> Result<u32, AccessPortError> {
        ADIMemoryInterface::new(0).read32(self, address)
    }

    fn read8(&mut self, address: u32) -> Result<u8, AccessPortError> {
        ADIMemoryInterface::new(0).read8(self, address)
    }

    fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).read_block32(self, address, data)
    }

    fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).read_block8(self, address, data)
    }

    fn write32(&mut self, addr: u32, data: u32) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).write32(self, addr, data)
    }

    fn write8(&mut self, addr: u32, data: u8) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).write8(self, addr, data)
    }

    fn write_block32(&mut self, addr: u32, data: &[u32]) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).write_block32(self, addr, data)
    }

    fn write_block8(&mut self, addr: u32, data: &[u8]) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).write_block8(self, addr, data)
    }
}

pub trait DebugProbe: DAPAccess + Send + Sync {
    fn new_from_probe_info(info: &DebugProbeInfo) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized;

    /// Get human readable name for the probe
    fn get_name(&self) -> &str;

    /// Enters debug mode
    fn attach(&mut self, protocol: Option<WireProtocol>) -> Result<WireProtocol, DebugProbeError>;

    /// Leave debug mode
    fn detach(&mut self) -> Result<(), DebugProbeError>;

    /// Resets the target device.
    fn target_reset(&mut self) -> Result<(), DebugProbeError>;
}

#[derive(Debug, Clone)]
pub enum DebugProbeType {
    DAPLink,
    STLink,
}

#[derive(Clone)]
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
            f,
            "{} (VID: {}, PID: {}, {}{:?})",
            self.identifier,
            self.vendor_id,
            self.product_id,
            self.serial_number
                .clone()
                .map_or("".to_owned(), |v| format!("Serial: {},", v)),
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
        probe_type: DebugProbeType,
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

#[derive(Default)]
pub struct FakeProbe;

impl FakeProbe {
    pub fn new() -> Self {
        Self::default()
    }
}

impl DebugProbe for FakeProbe {
    fn new_from_probe_info(_info: &DebugProbeInfo) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized,
    {
        Err(DebugProbeError::ProbeCouldNotBeCreated)
    }

    /// Get human readable name for the probe
    fn get_name(&self) -> &str {
        "Mock probe for testing"
    }

    /// Enters debug mode
    fn attach(&mut self, protocol: Option<WireProtocol>) -> Result<WireProtocol, DebugProbeError> {
        // attaching always work for the fake probe
        Ok(protocol.unwrap_or(WireProtocol::Swd))
    }

    /// Leave debug mode
    fn detach(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    /// Resets the target device.
    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::UnknownError)
    }
}

impl DAPAccess for FakeProbe {
    /// Reads the DAP register on the specified port and address
    fn read_register(&mut self, _port: Port, _addr: u16) -> Result<u32, DebugProbeError> {
        Err(DebugProbeError::UnknownError)
    }

    /// Writes a value to the DAP register on the specified port and address
    fn write_register(
        &mut self,
        _port: Port,
        _addr: u16,
        _value: u32,
    ) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::UnknownError)
    }
}
