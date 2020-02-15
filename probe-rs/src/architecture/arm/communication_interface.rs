use super::{
    ap::{
        valid_access_ports, APAccess, APClass, APRegister, AccessPort, BaseaddrFormat, GenericAP,
        MemoryAP, BASE, BASE2, IDR,
    },
    dp::Select,
    memory::romtable::{ComponentId, ComponentClass, RomTable, RomTableEntry},
};
use crate::config::ChipInfo;
use crate::{CommunicationInterface, Core, DebugProbe, DebugProbeError, Error, Memory, Probe};
use jep106::JEP106Code;
use log::debug;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum PortType {
    DebugPort,
    AccessPort(u16),
}

impl From<u16> for PortType {
    fn from(value: u16) -> PortType {
        if value == 0xFFFF {
            PortType::DebugPort
        } else {
            PortType::AccessPort(value)
        }
    }
}

impl From<PortType> for u16 {
    fn from(value: PortType) -> u16 {
        match value {
            PortType::DebugPort => 0xFFFF,
            PortType::AccessPort(value) => value,
        }
    }
}
use std::fmt::Debug;

pub trait Register: Clone + From<u32> + Into<u32> + Sized + Debug {
    const ADDRESS: u8;
    const NAME: &'static str;
}

pub trait DAPAccess: DebugProbe {
    /// Reads the DAP register on the specified port and address
    fn read_register(&mut self, port: PortType, addr: u16) -> Result<u32, DebugProbeError>;

    /// Read multiple values from the same DAP register.
    ///
    /// If possible, this uses optimized read functions, otherwise it
    /// falls back to the `read_register` function.
    fn read_block(
        &mut self,
        port: PortType,
        addr: u16,
        values: &mut [u32],
    ) -> Result<(), DebugProbeError> {
        for val in values {
            *val = self.read_register(port, addr)?;
        }

        Ok(())
    }

    /// Writes a value to the DAP register on the specified port and address
    fn write_register(
        &mut self,
        port: PortType,
        addr: u16,
        value: u32,
    ) -> Result<(), DebugProbeError>;

    /// Write multiple values to the same DAP register.
    ///
    /// If possible, this uses optimized write functions, otherwise it
    /// falls back to the `write_register` function.
    fn write_block(
        &mut self,
        port: PortType,
        addr: u16,
        values: &[u32],
    ) -> Result<(), DebugProbeError> {
        for val in values {
            self.write_register(port, addr, *val)?;
        }

        Ok(())
    }
}

#[derive(Clone)]
pub struct ArmCommunicationInterface {
    inner: Rc<RefCell<InnerArmCommunicationInterface>>,
}

impl ArmCommunicationInterface {
    pub fn new(probe: Probe) -> Self {
        Self {
            inner: Rc::new(RefCell::new(InnerArmCommunicationInterface::new(probe))),
        }
    }

    pub fn dedicated_memory_interface(&self) -> Option<Memory> {
        self.inner.borrow().probe.dedicated_memory_interface()
    }

    pub fn read_register_dp(&mut self, offset: u16) -> Result<u32, DebugProbeError> {
        self.inner.borrow_mut().read_register_dp(offset)
    }

    pub fn write_register_dp(&mut self, offset: u16, val: u32) -> Result<(), DebugProbeError> {
        self.inner.borrow_mut().write_register_dp(offset, val)
    }

    pub fn read_swv(&mut self) -> Result<Vec<u8>, Error> {
        self.inner.borrow_mut().read_swv()
    }

    /// This function should be called once when initializing the object.
    /// It stores all the memory AP IDs and BASEADDRs in a Vec.
    pub(crate) fn memory_access_ports(&mut self) -> Result<Vec<MemoryAccessPortData>, Error> {
        if self.inner.borrow().memory_access_ports.is_empty() {
            let mut memory_access_ports = vec![];
            for access_port in valid_access_ports(self) {
                println!("VALID: {:?}", access_port.port_number());
                let idr = self
                    .read_ap_register(access_port, IDR::default())
                    .map_err(Error::Probe)?;

                if idr.CLASS == APClass::MEMAP {
                    println!("MEMORY AP");
                    let access_port: MemoryAP = access_port.into();

                    let base_register = self
                        .read_ap_register(access_port, BASE::default())
                        .map_err(Error::Probe)?;

                    let mut base_address = if BaseaddrFormat::ADIv5 == base_register.Format {
                        let base2 = self
                            .read_ap_register(access_port, BASE2::default())
                            .map_err(Error::Probe)?;
                        (u64::from(base2.BASEADDR) << 32)
                    } else {
                        0
                    };
                    base_address |= u64::from(base_register.BASEADDR << 12);

                    memory_access_ports.push(MemoryAccessPortData {
                        id: access_port.port_number(),
                        base_address,
                    })
                }
            }
            self.inner.borrow_mut().memory_access_ports = memory_access_ports.clone();
            Ok(memory_access_ports)
        } else {
            Ok(self.inner.borrow().memory_access_ports.clone())
        }
    }
}

struct InnerArmCommunicationInterface {
    probe: Probe,
    memory_access_ports: Vec<MemoryAccessPortData>,
    current_apsel: u8,
    current_apbanksel: u8,
}

impl InnerArmCommunicationInterface {
    fn new(probe: Probe) -> Self {
        let interface = Self {
            probe,
            memory_access_ports: vec![],
            current_apsel: 0,
            current_apbanksel: 0,
        };
        interface
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
            let mut select = Select(0);

            debug!(
                "Changing AP to {}, AP_BANK_SEL to {}",
                self.current_apsel, self.current_apbanksel
            );

            select.set_ap_sel(self.current_apsel);
            select.set_ap_bank_sel(self.current_apbanksel);

            let interface = self
                .probe
                .get_interface_dap_mut()
                .ok_or_else(|| DebugProbeError::InterfaceNotAvailable("ARM"))?;

            interface.write_register(
                PortType::DebugPort,
                u16::from(Select::ADDRESS),
                select.into(),
            )?;
        }

        Ok(())
    }

    fn write_ap_register<AP, R>(&mut self, port: impl Into<AP>, register: R) -> Result<(), DebugProbeError>
    where
        AP: AccessPort,
        R: APRegister<AP>,
    {
        let register_value = register.into();

        debug!(
            "Writing register {}, value=0x{:08X}",
            R::NAME,
            register_value
        );

        self.select_ap_and_ap_bank(port.into().port_number(), R::APBANKSEL)?;

        let interface = self
            .probe
            .get_interface_dap_mut()
            .ok_or_else(|| DebugProbeError::InterfaceNotAvailable("ARM"))?;

        interface.write_register(
            PortType::AccessPort(u16::from(self.current_apsel)),
            u16::from(R::ADDRESS),
            register_value,
        )?;
        Ok(())
    }

    /// TODO: Fix this ugly: _register: R, values: &[u32]
    fn write_ap_register_repeated<AP, R>(
        &mut self,
        port: impl Into<AP>,
        _register: R,
        values: &[u32],
    ) -> Result<(), DebugProbeError>
    where
        AP: AccessPort,
        R: APRegister<AP>,
    {
        debug!(
            "Writing register {}, block with len={} words",
            R::NAME,
            values.len(),
        );

        self.select_ap_and_ap_bank(port.into().port_number(), R::APBANKSEL)?;

        let interface = self
            .probe
            .get_interface_dap_mut()
            .ok_or_else(|| DebugProbeError::InterfaceNotAvailable("ARM"))?;

        interface.write_block(
            PortType::AccessPort(u16::from(self.current_apsel)),
            u16::from(R::ADDRESS),
            values,
        )?;
        Ok(())
    }

    fn read_ap_register<AP, R>(&mut self, port: impl Into<AP>, _register: R) -> Result<R, DebugProbeError>
    where
        AP: AccessPort,
        R: APRegister<AP>,
    {
        debug!("Reading register {}", R::NAME);
        self.select_ap_and_ap_bank(port.into().port_number(), R::APBANKSEL)?;

        let interface = self
            .probe
            .get_interface_dap_mut()
            .ok_or_else(|| DebugProbeError::InterfaceNotAvailable("ARM"))?;

        let result = interface.read_register(
            PortType::AccessPort(u16::from(self.current_apsel)),
            u16::from(R::ADDRESS),
        )?;

        debug!("Read register    {}, value=0x{:08x}", R::NAME, result);

        Ok(R::from(result))
    }

    /// TODO: fix types, see above!
    fn read_ap_register_repeated<AP, R>(
        &mut self,
        port: impl Into<AP>,
        _register: R,
        values: &mut [u32],
    ) -> Result<(), DebugProbeError>
    where
        AP: AccessPort,
        R: APRegister<AP>,
    {
        debug!(
            "Reading register {}, block with len={} words",
            R::NAME,
            values.len(),
        );

        self.select_ap_and_ap_bank(port.into().port_number(), R::APBANKSEL)?;

        let interface = self
            .probe
            .get_interface_dap_mut()
            .ok_or_else(|| DebugProbeError::InterfaceNotAvailable("ARM"))?;

        interface.read_block(
            PortType::AccessPort(u16::from(self.current_apsel)),
            u16::from(R::ADDRESS),
            values,
        )?;
        Ok(())
    }

    fn read_register_dp(&mut self, offset: u16) -> Result<u32, DebugProbeError> {
        let interface = self
            .probe
            .get_interface_dap_mut()
            .ok_or_else(|| DebugProbeError::InterfaceNotAvailable("ARM"))?;

        interface.read_register(PortType::DebugPort, offset)
    }

    fn write_register_dp(&mut self, offset: u16, val: u32) -> Result<(), DebugProbeError> {
        let interface = self
            .probe
            .get_interface_dap_mut()
            .ok_or_else(|| DebugProbeError::InterfaceNotAvailable("ARM"))?;

        interface.write_register(PortType::DebugPort, offset, val)
    }

    fn read_swv(&mut self) -> Result<Vec<u8>, Error> {
        match self.probe.get_interface_itm_mut() {
            Some(interface) => interface.read(),
            None => Err(Error::WouldBlock),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MemoryAccessPortData {
    id: u8,
    base_address: u64,
}

impl MemoryAccessPortData {
    pub fn id(&self) -> u8 {
        self.id
    }

    pub fn base_address(&self) -> u64 {
        self.base_address
    }
}

impl CommunicationInterface for ArmCommunicationInterface {
    fn probe_for_chip_info(mut self, core: &mut Core) -> Result<Option<ChipInfo>, Error> {
        ArmChipInfo::read_from_rom_table(core, &mut self).map(|option| option.map(ChipInfo::Arm))
    }
}

impl<R> APAccess<MemoryAP, R> for ArmCommunicationInterface
where
    R: APRegister<MemoryAP>,
{
    type Error = DebugProbeError;

    fn read_ap_register(&mut self, port: impl Into<MemoryAP>, register: R) -> Result<R, Self::Error> {
        self.inner.borrow_mut().read_ap_register(port, register)
    }

    fn write_ap_register(&mut self, port: impl Into<MemoryAP>, register: R) -> Result<(), Self::Error> {
        self.inner.borrow_mut().write_ap_register(port, register)
    }

    fn write_ap_register_repeated(
        &mut self,
        port: impl Into<MemoryAP>,
        register: R,
        values: &[u32],
    ) -> Result<(), Self::Error> {
        self.inner
            .borrow_mut()
            .write_ap_register_repeated(port, register, values)
    }

    fn read_ap_register_repeated(
        &mut self,
        port: impl Into<MemoryAP>,
        register: R,
        values: &mut [u32],
    ) -> Result<(), Self::Error> {
        self.inner
            .borrow_mut()
            .read_ap_register_repeated(port, register, values)
    }
}

impl<R> APAccess<GenericAP, R> for ArmCommunicationInterface
where
    R: APRegister<GenericAP>,
{
    type Error = DebugProbeError;

    fn read_ap_register(&mut self, port: impl Into<GenericAP>, register: R) -> Result<R, Self::Error> {
        self.inner.borrow_mut().read_ap_register(port, register)
    }

    fn write_ap_register(&mut self, port: impl Into<GenericAP>, register: R) -> Result<(), Self::Error> {
        self.inner.borrow_mut().write_ap_register(port, register)
    }

    fn write_ap_register_repeated(
        &mut self,
        port: impl Into<GenericAP>,
        register: R,
        values: &[u32],
    ) -> Result<(), Self::Error> {
        self.inner
            .borrow_mut()
            .write_ap_register_repeated(port, register, values)
    }

    fn read_ap_register_repeated(
        &mut self,
        port: impl Into<GenericAP>,
        register: R,
        values: &mut [u32],
    ) -> Result<(), Self::Error> {
        self.inner
            .borrow_mut()
            .read_ap_register_repeated(port, register, values)
    }
}

#[derive(Debug)]
pub struct ArmChipInfo {
    pub manufacturer: JEP106Code,
    pub part: u16,
}

impl ArmChipInfo {
    pub fn read_from_rom_table(
        core: &mut Core,
        interface: &mut ArmCommunicationInterface,
    ) -> Result<Option<Self>, Error> {
        for access_port in valid_access_ports(interface) {
            let idr = interface
                .read_ap_register(access_port, IDR::default())
                .map_err(Error::Probe)?;
            debug!("{:#x?}", idr);

            if idr.CLASS == APClass::MEMAP {
                let access_port: MemoryAP = access_port.into();

                let base_register = interface
                    .read_ap_register(access_port, BASE::default())
                    .map_err(Error::Probe)?;

                let mut baseaddr = if BaseaddrFormat::ADIv5 == base_register.Format {
                    let base2 = interface
                        .read_ap_register(access_port, BASE2::default())
                        .map_err(Error::Probe)?;
                    (u64::from(base2.BASEADDR) << 32)
                } else {
                    0
                };
                baseaddr |= u64::from(base_register.BASEADDR << 12);

                let rom_table = RomTable::try_parse(core, baseaddr as u64)
                    .map_err(Error::architecture_specific)?;

                let mut rom_table_entries = rom_table.entries();

                if let Some(RomTableEntry {
                    component_data: ComponentClass::Class1RomTable(_),
                    component_id: ComponentId { peripheral_id, .. },
                    ..
                }) = rom_table_entries.next()
                {
                    if let Some(jep106) = peripheral_id.jep106() {
                        return Ok(Some(ArmChipInfo {
                            manufacturer: jep106,
                            part: peripheral_id.part(),
                        }));
                    }
                }
            }
        }
        // log::info!(
        //     "{}\n{}\n{}\n{}",
        //     "If you are using a Nordic chip, it might be locked to debug access".yellow(),
        //     "Run cargo flash with --nrf-recover to unlock".yellow(),
        //     "WARNING: --nrf-recover will erase the entire code".yellow(),
        //     "flash and UICR area of the device, in addition to the entire RAM".yellow()
        // );

        Ok(None)
    }
}

impl std::fmt::Display for ArmChipInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let manu = match self.manufacturer.get() {
            Some(name) => name.to_string(),
            None => format!(
                "<unknown manufacturer (cc={:2x}, id={:2x})>",
                self.manufacturer.cc, self.manufacturer.id
            ),
        };
        write!(f, "{} 0x{:04x}", manu, self.part)
    }
}
