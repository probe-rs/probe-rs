use std::collections::HashMap;
use crate::ap_access::APAccess;
use super::{
    MemoryAP,
    APRegister,
    CSW,
    DataSize,
    TAR,
    DRW,
};
use crate::common::Register;

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
        let mut store = HashMap::new();
        store.insert((CSW::ADDRESS, CSW::APBANKSEL), 0);
        store.insert((TAR::ADDRESS, TAR::APBANKSEL), 0);
        store.insert((DRW::ADDRESS, DRW::APBANKSEL), 0);
        Self {
            data: vec![0; 256],
            store,
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
        println!("read");
        let csw = *self.store.get(&(CSW::ADDRESS, CSW::APBANKSEL)).unwrap();
        let address = *self.store.get(&(TAR::ADDRESS, TAR::APBANKSEL)).unwrap();
        println!("csw: {:08x}", csw);
        match (REGISTER::ADDRESS, REGISTER::APBANKSEL) {
            (DRW::ADDRESS, DRW::APBANKSEL) => match CSW::from(csw).SIZE {
                DataSize::U32 => Ok(REGISTER::from(
                      self.data[address as usize + 0] as u32 |
                    ((self.data[address as usize + 1] as u32) << 08) |
                    ((self.data[address as usize + 2] as u32) << 16) |
                    ((self.data[address as usize + 3] as u32) << 24)
                )),
                DataSize::U16 => {
                    println!("{:?}", self.data);
                    Ok(REGISTER::from(
                      self.data[address as usize + 0] as u32 |
                    ((self.data[address as usize + 1] as u32) << 08)
                ))
                },
                DataSize::U8 => Ok(REGISTER::from(self.data[address as usize + 0] as u32 )),
                _ => Err(MockMemoryError::UnknownWidth)
            },
            (CSW::ADDRESS, CSW::APBANKSEL) => Ok(REGISTER::from(*self.store.get(&(REGISTER::ADDRESS, REGISTER::APBANKSEL)).unwrap())),
            (TAR::ADDRESS, TAR::APBANKSEL) => Ok(REGISTER::from(*self.store.get(&(REGISTER::ADDRESS, REGISTER::APBANKSEL)).unwrap())),
            _ => Err(MockMemoryError::UnknownRegister)
        }
    }

    /// Mocks the write_register method of a AP.
    /// 
    /// Returns an Error if any bad instructions or values are chosen.
    fn write_register_ap(&mut self, _port: MemoryAP, register: REGISTER) -> Result<(), Self::Error> {
        println!("write");
        let value = register.into();
        println!("{:?}", (REGISTER::ADDRESS, REGISTER::APBANKSEL));
        self.store.insert((REGISTER::ADDRESS, REGISTER::APBANKSEL), value);
        let csw = *self.store.get(&(CSW::ADDRESS, CSW::APBANKSEL)).unwrap();
        println!("csw: {:08x}", csw);
        let address = *self.store.get(&(TAR::ADDRESS, TAR::APBANKSEL)).unwrap();
        println!("address: {:08x}", address);
        println!("{:?}", CSW::from(csw));
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
            (CSW::ADDRESS, CSW::APBANKSEL) => Ok(()),
            (TAR::ADDRESS, TAR::APBANKSEL) => Ok(()),
            _ => Err(MockMemoryError::UnknownRegister)
        }
    }
}