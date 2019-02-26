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
    store: HashMap<(u8, u8), u32>,
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
        let csw = self.store[&(CSW::ADDRESS, CSW::APBANKSEL)];
        let address = self.store[&(TAR::ADDRESS, TAR::APBANKSEL)];
        match (REGISTER::ADDRESS, REGISTER::APBANKSEL) {
            (DRW::ADDRESS, DRW::APBANKSEL) => match CSW::from(csw).SIZE {
                DataSize::U32 => Ok(REGISTER::from(
                     u32::from(self.data[address as usize    ])        |
                    (u32::from(self.data[address as usize + 1]) <<  8) |
                    (u32::from(self.data[address as usize + 2]) << 16) |
                    (u32::from(self.data[address as usize + 3]) << 24)
                )),
                DataSize::U16 => Ok(REGISTER::from(
                     u32::from(self.data[address as usize    ])         |
                    (u32::from(self.data[address as usize + 1]) <<  8)
                )),
                DataSize::U8 => Ok(REGISTER::from(
                     u32::from(self.data[address as usize    ])
                )),
                _ => Err(MockMemoryError::UnknownWidth)
            },
            (CSW::ADDRESS, CSW::APBANKSEL) => Ok(REGISTER::from(self.store[&(REGISTER::ADDRESS, REGISTER::APBANKSEL)])),
            (TAR::ADDRESS, TAR::APBANKSEL) => Ok(REGISTER::from(self.store[&(REGISTER::ADDRESS, REGISTER::APBANKSEL)])),
            _ => Err(MockMemoryError::UnknownRegister)
        }
    }

    /// Mocks the write_register method of a AP.
    /// 
    /// Returns an Error if any bad instructions or values are chosen.
    fn write_register_ap(&mut self, _port: MemoryAP, register: REGISTER) -> Result<(), Self::Error> {
        let value = register.into();
        self.store.insert((REGISTER::ADDRESS, REGISTER::APBANKSEL), value);
        let csw = self.store[&(CSW::ADDRESS, CSW::APBANKSEL)];
        let address = self.store[&(TAR::ADDRESS, TAR::APBANKSEL)];
        match (REGISTER::ADDRESS, REGISTER::APBANKSEL) {
            (DRW::ADDRESS, DRW::APBANKSEL) => match CSW::from(csw).SIZE {
                DataSize::U32 => {
                    self.data[address as usize    ] =  value        as u8;
                    self.data[address as usize + 1] = (value >>  8) as u8;
                    self.data[address as usize + 2] = (value >> 16) as u8;
                    self.data[address as usize + 3] = (value >> 24) as u8;
                    Ok(())
                },
                DataSize::U16 => {
                    self.data[address as usize    ] =  value        as u8;
                    self.data[address as usize + 1] = (value >>  8) as u8;
                    Ok(())
                },
                DataSize::U8 => {
                    self.data[address as usize    ] =  value        as u8;
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