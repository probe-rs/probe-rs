use std::collections::HashMap;
use crate::access_ports::memory_ap::{
    TAR,
    CSW,
    DRW,
    MemoryAP,
    DataSize,
};
use crate::common::Register;
use crate::access_ports::{
    APType,
    APRegister,
};

pub trait APAccess<PORT, REGISTER>
where
    PORT: APType,
    REGISTER: APRegister<PORT>,
{
    type Error;
    fn read_register_ap(&mut self, port: PORT, register: REGISTER) -> Result<REGISTER, Self::Error>;
    fn write_register_ap(&mut self, port: PORT, register: REGISTER) -> Result<(), Self::Error>;
}

pub struct MockMemoryAP {
    pub data: Vec<u8>,
    store: HashMap<(u16, u8), u32>,
}

#[derive(Debug)]
pub enum MockMemoryError {
    UnknownWidth,
    UnknownRegister,
}

impl MockMemoryAP {
    pub fn new() -> Self {
        Self {
            data: vec![0; 256],
            store: HashMap::new(),
        }
    }
}

impl<REGISTER> APAccess<MemoryAP, REGISTER> for MockMemoryAP
where
    REGISTER: APRegister<MemoryAP>
{
    type Error = MockMemoryError;

    /// Mocks the read_register method of a AP.
    /// 
    /// Returns an Error if any bad instructions or values are chosen.
    fn read_register_ap(&mut self, _port: MemoryAP, _register: REGISTER) -> Result<REGISTER, Self::Error> {
        let value = *self.store.get(&(REGISTER::ADDRESS, REGISTER::APBANKSEL)).unwrap();
        let address = *self.store.get(&(TAR::ADDRESS, TAR::APBANKSEL)).unwrap();
        match (REGISTER::ADDRESS, REGISTER::APBANKSEL) {
            (CSW::ADDRESS, CSW::APBANKSEL) => match CSW::from(value).SIZE {
                DataSize::U32 => Ok(REGISTER::from(
                      self.data[address as usize + 0] as u32 |
                    ((self.data[address as usize + 1] as u32) << 08) |
                    ((self.data[address as usize + 2] as u32) << 16) |
                    ((self.data[address as usize + 3] as u32) << 24)
                )),
                DataSize::U16 => Ok(REGISTER::from(
                      self.data[address as usize + 0] as u32 |
                    ((self.data[address as usize + 1] as u32) << 08)
                )),
                DataSize::U8 => Ok(REGISTER::from(self.data[address as usize + 0] as u32 )),
                _ => Err(MockMemoryError::UnknownWidth)
            },
            _ => Err(MockMemoryError::UnknownRegister)
        }
    }

    /// Mocks the write_register method of a AP.
    /// 
    /// Returns an Error if any bad instructions or values are chosen.
    fn write_register_ap(&mut self, _port: MemoryAP, register: REGISTER) -> Result<(), Self::Error> {
        let value = register.into();
        self.store.insert((REGISTER::ADDRESS, REGISTER::APBANKSEL), value);
        let csw = *self.store.get(&(CSW::ADDRESS, CSW::APBANKSEL)).unwrap();
        let address = *self.store.get(&(TAR::ADDRESS, TAR::APBANKSEL)).unwrap();
        match (REGISTER::ADDRESS, REGISTER::APBANKSEL) {
            (DRW::ADDRESS, DRW::APBANKSEL) => match CSW::from(csw).SIZE {
                DataSize::U32 => {
                    self.data[address as usize + 0] = (value >> 00) as u8;
                    self.data[address as usize + 1] = (value >> 08) as u8;
                    self.data[address as usize + 2] = (value >> 16) as u8;
                    self.data[address as usize + 3] = (value >> 24) as u8;
                    Ok(())
                },
                DataSize::U16 => {
                    self.data[address as usize + 0] = (value >> 00) as u8;
                    self.data[address as usize + 1] = (value >> 08) as u8;
                    Ok(())
                },
                DataSize::U8 => {
                    self.data[address as usize + 0] = (value >> 00) as u8;
                    Ok(())
                },
                _ => Err(MockMemoryError::UnknownWidth)
            },
            _ => Err(MockMemoryError::UnknownRegister)
        }
    }
}