use super::access_port::consts::*;

pub trait DAPAccess {
    type Error: std::fmt::Debug;

    /// Reads the DAP register on the specified port and address
    fn read_register(&mut self, port: u16, addr: u32) -> Result<u32, Self::Error>;

    /// Writes a value to the DAP register on the specified port and address
    fn write_register(&mut self, port: u16, addr: u32, value: u32) -> Result<(), Self::Error>;
}

pub struct MockDAP {
    pub data: Vec<u8>,
    width: u32,
    address: u32,
}

#[derive(Debug)]
pub enum MockError {
    BadWidth,
    BadInstruction,
}

impl MockDAP {
    pub fn new() -> Self {
        Self {
            data: vec![0; 256],
            width: 4,
            address: 0,
        }
    }
}

impl DAPAccess for MockDAP {
    type Error = MockError;

    /// Mocks the read_register method of a DAP.
    /// 
    /// Returns an Error if any bad instructions or values are chosen.
    fn read_register(&mut self, _port: u16, addr: u32) -> Result<u32, Self::Error> {
        if addr == MEM_AP_CSW {
            Ok(if self.width == 0 {
                0
            } else if self.width == 1 {
                2
            } else {
                4
            })
        } else if addr == MEM_AP_TAR {
            Ok(self.address)
        } else if addr == MEM_AP_DRW {
            if self.width == 4 {
                Ok(
                    self.data[self.address as usize + 0] as u32 |
                    ((self.data[self.address as usize + 1] as u32) << 08) |
                    ((self.data[self.address as usize + 2] as u32) << 16) |
                    ((self.data[self.address as usize + 3] as u32) << 24)
                )
            } else if self.width == 2 {
                Ok(
                    self.data[self.address as usize + 0] as u32 |
                    ((self.data[self.address as usize + 1] as u32) << 08)
                )
            } else {
                Ok(self.data[self.address as usize + 0] as u32)
            }
        } else {
            Err(MockError::BadInstruction)
        }
    }

    /// Mocks the write_register method of a DAP.
    /// 
    /// Returns an Error if any bad instructions or values are chosen.
    fn write_register(&mut self, _port: u16, addr: u32, value: u32) -> Result<(), Self::Error> {
        if addr == MEM_AP_CSW {
            if value & 0x3 == 0 {
                self.width = 1;
            } else if value & 0x3 == 1 {
                self.width = 2;
            } else if value & 0x3 == 2 {
                self.width = 4;
            } else {
                return Err(MockError::BadWidth);
            }
            Ok(())
        } else if addr == MEM_AP_TAR {
            self.address = value;
            Ok(())
        } else if addr == MEM_AP_DRW {
            if self.width == 4 {
                self.data[self.address as usize + 0] = (value >> 00) as u8;
                self.data[self.address as usize + 1] = (value >> 08) as u8;
                self.data[self.address as usize + 2] = (value >> 16) as u8;
                self.data[self.address as usize + 3] = (value >> 24) as u8;
            } else if self.width == 2 {
                self.data[self.address as usize + 0] = (value >> 00) as u8;
                self.data[self.address as usize + 1] = (value >> 08) as u8;
            } else {
                self.data[self.address as usize + 0] = (value >> 00) as u8;
            }
            Ok(())
        } else {
            Err(MockError::BadInstruction)
        }
    }
}