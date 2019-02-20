use crate::access_ports::memory_ap::TAR;
use crate::access_ports::memory_ap::CSW;
use crate::access_ports::memory_ap::MemoryAPValue;
use crate::access_ports::memory_ap::MemoryAPRegister;
use crate::access_ports::memory_ap::MemoryAP;
use crate::access_ports::memory_ap::DataSize;
use super::access_port::consts::*;
use crate::access_ports::APRegister;
use crate::access_ports::APValue;

pub trait APAccess<PORT, REGISTER, VALUE> {
    type Error;
    fn read_register_ap(&mut self, port: PORT, register: REGISTER) -> Result<VALUE, Self::Error>;
    fn write_register_ap(&mut self, port: PORT, register: REGISTER, value: VALUE) -> Result<(), Self::Error>;
}

pub struct MockMemoryAP {
    pub data: Vec<u8>,
    csw: CSW,
    tar: TAR,
    address: u32,
}

#[derive(Debug)]
pub enum MockMemoryError {
    BadWidth,
    BadInstruction,
}

impl MockMemoryAP {
    pub fn new() -> Self {
        Self {
            data: vec![0; 256],
            csw: Default::default(),
            tar: Default::default(),
            address: 0,
        }
    }
}

impl APAccess<MemoryAP, MemoryAPRegister, MemoryAPValue> for MockMemoryAP {
    type Error = MockMemoryError;

    /// Mocks the read_register method of a AP.
    /// 
    /// Returns an Error if any bad instructions or values are chosen.
    fn read_register_ap(&mut self, _port: MemoryAP, addr: MemoryAPRegister) -> Result<MemoryAPValue, Self::Error> {
        use MemoryAPRegister as R;
        use MemoryAPValue as V;
        match addr {
            R::CSW =>
                Ok(V::CSW(self.csw)),
            R::TAR0 => Ok(V::TAR0(self.tar)),
            R::DRW => match self.csw.SIZE {
                DataSize::U32 => Ok(V::TAR0(TAR { address:
                    self.data[self.tar.address as usize + 0] as u32 |
                    ((self.data[self.address as usize + 1] as u32) << 08) |
                    ((self.data[self.address as usize + 2] as u32) << 16) |
                    ((self.data[self.address as usize + 3] as u32) << 24)
                })),
                DataSize::U16 => Ok(V::TAR0(TAR { address:
                    self.data[self.address as usize + 0] as u32 |
                    ((self.data[self.address as usize + 1] as u32) << 08)
                })),
                DataSize::U8 => Ok(V::TAR0(TAR { address:self.data[self.address as usize + 0] as u32 })),
                _ => Err(MockMemoryError::BadWidth)
            }
        }
    }

    /// Mocks the write_register method of a AP.
    /// 
    /// Returns an Error if any bad instructions or values are chosen.
    fn write_register_ap(&mut self, _port: MemoryAP, addr: MemoryAPRegister, value: MemoryAPValue) -> Result<(), Self::Error> {
        use MemoryAPRegister as R;
        use MemoryAPValue as V;
        match addr {
            R::CSW => match value {
                V::CSW(v) => { self.csw = v; Ok(()) },
                _ => Err(MockMemoryError::BadWidth)
            },
            R::TAR0 => match value {
                V::TAR0(v) => { self.tar = v; Ok(()) },
                _ => Err(MockMemoryError::BadWidth)
            },
            R::DRW => match self.csw.SIZE {
                DataSize::U32 => {
                    let v = match value {
                        V::DRW(v) => { v },
                        _ => return Err(MockMemoryError::BadWidth)
                    };
                    self.data[self.address as usize + 0] = (v.data >> 00) as u8;
                    self.data[self.address as usize + 1] = (v.data >> 08) as u8;
                    self.data[self.address as usize + 2] = (v.data >> 16) as u8;
                    self.data[self.address as usize + 3] = (v.data >> 24) as u8;
                    Ok(())
                },
                DataSize::U16 => {
                    let v = match value {
                        V::DRW(v) => { v },
                        _ => return Err(MockMemoryError::BadWidth)
                    };
                    self.data[self.address as usize + 0] = (v.data >> 00) as u8;
                    self.data[self.address as usize + 1] = (v.data >> 08) as u8;
                    Ok(())
                },
                DataSize::U8 => {
                    let v = match value {
                        V::DRW(v) => { v },
                        _ => return Err(MockMemoryError::BadWidth)
                    };
                    self.data[self.address as usize + 0] = (v.data >> 00) as u8;
                    Ok(())
                },
                _ => Err(MockMemoryError::BadWidth)
            }
        }
    }
}