use super::{
    ap::{
        valid_access_ports, APAccess, APClass, APRegister, AccessPort, BaseaddrFormat, GenericAP,
        MemoryAP, BASE, BASE2, IDR,
    },
    dp::Select,
    memory::romtable::{CSComponent, CSComponentId, PeripheralID},
    memory::ADIMemoryInterface,
};
use crate::config::ChipInfo;
use crate::{CommunicationInterface, DebugProbe, DebugProbeError, Error, Memory, Probe};
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
        let mut i = 0;
        for val in values {
            println!("{}", i);
            i+=1;
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
    pub fn new(probe: Probe) -> Result<Self, DebugProbeError> {
        Ok(Self {
            inner: Rc::new(RefCell::new(InnerArmCommunicationInterface::new(probe)?)),
        })
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

    pub fn close(self) -> Probe {
        let inner = Rc::try_unwrap(self.inner);

        match inner {
            Ok(inner) => inner.into_inner().probe,
            Err(_) => {
                panic!("Failed to close ArmCommunicationInterface");
            }
        }
    }
}

struct InnerArmCommunicationInterface {
    probe: Probe,
    current_apsel: u8,
    current_apbanksel: u8,
}

impl InnerArmCommunicationInterface {
    fn new(probe: Probe) -> Result<Self, DebugProbeError> {
        // TODO: It would be nice if could store a DapInterface directly, so we don't have to get
        //       it everytime we want to access it.
        if probe.get_interface_dap().is_none() {
            return Err(DebugProbeError::InterfaceNotAvailable("ARM"));
        }

        Ok(Self {
            probe,
            current_apsel: 0,
            current_apbanksel: 0,
        })
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

    fn write_ap_register<AP, R>(&mut self, port: AP, register: R) -> Result<(), DebugProbeError>
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

        self.select_ap_and_ap_bank(port.get_port_number(), R::APBANKSEL)?;

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
        port: AP,
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

        self.select_ap_and_ap_bank(port.get_port_number(), R::APBANKSEL)?;

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

    fn read_ap_register<AP, R>(&mut self, port: AP, _register: R) -> Result<R, DebugProbeError>
    where
        AP: AccessPort,
        R: APRegister<AP>,
    {
        debug!("Reading register {}", R::NAME);
        self.select_ap_and_ap_bank(port.get_port_number(), R::APBANKSEL)?;

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
        port: AP,
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

        self.select_ap_and_ap_bank(port.get_port_number(), R::APBANKSEL)?;
        println!("1");

        let interface = self
            .probe
            .get_interface_dap_mut()
            .ok_or_else(|| DebugProbeError::InterfaceNotAvailable("ARM"))?;
        println!("2");
        interface.read_block(
            PortType::AccessPort(u16::from(self.current_apsel)),
            u16::from(R::ADDRESS),
            values,
        )?;
        println!("3");
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
}

impl CommunicationInterface for ArmCommunicationInterface {
    fn probe_for_chip_info(mut self) -> Result<Option<ChipInfo>, Error> {
        ArmChipInfo::read_from_rom_table(&mut self).map(|option| option.map(ChipInfo::Arm))
    }
}

impl<R> APAccess<MemoryAP, R> for ArmCommunicationInterface
where
    R: APRegister<MemoryAP>,
{
    type Error = DebugProbeError;

    fn read_ap_register(&mut self, port: MemoryAP, register: R) -> Result<R, Self::Error> {
        self.inner.borrow_mut().read_ap_register(port, register)
    }

    fn write_ap_register(&mut self, port: MemoryAP, register: R) -> Result<(), Self::Error> {
        self.inner.borrow_mut().write_ap_register(port, register)
    }

    fn write_ap_register_repeated(
        &mut self,
        port: MemoryAP,
        register: R,
        values: &[u32],
    ) -> Result<(), Self::Error> {
        self.inner
            .borrow_mut()
            .write_ap_register_repeated(port, register, values)
    }

    fn read_ap_register_repeated(
        &mut self,
        port: MemoryAP,
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

    fn read_ap_register(&mut self, port: GenericAP, register: R) -> Result<R, Self::Error> {
        self.inner.borrow_mut().read_ap_register(port, register)
    }

    fn write_ap_register(&mut self, port: GenericAP, register: R) -> Result<(), Self::Error> {
        self.inner.borrow_mut().write_ap_register(port, register)
    }

    fn write_ap_register_repeated(
        &mut self,
        port: GenericAP,
        register: R,
        values: &[u32],
    ) -> Result<(), Self::Error> {
        self.inner
            .borrow_mut()
            .write_ap_register_repeated(port, register, values)
    }

    fn read_ap_register_repeated(
        &mut self,
        port: GenericAP,
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

                let memory = Memory::new(ADIMemoryInterface::<ArmCommunicationInterface>::new(
                    interface.clone(),
                    access_port,
                ));

                let component_table = CSComponent::try_parse(memory, baseaddr as u64)
                    .map_err(Error::architecture_specific)?;

                match component_table {
                    CSComponent::Class1RomTable(
                        CSComponentId {
                            peripheral_id:
                                PeripheralID {
                                    JEP106: Some(jep106),
                                    PART: part,
                                    ..
                                },
                            ..
                        },
                        ..,
                    ) => {
                        return Ok(Some(ArmChipInfo {
                            manufacturer: jep106,
                            part,
                        }));
                    }
                    _ => continue,
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
