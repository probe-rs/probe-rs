use crate::common::Register;
use num_traits::{
    FromPrimitive,
    ToPrimitive,
};
use enum_primitive_derive::Primitive;

use crate::access_ports::APRegister;
use crate::access_ports::APType;

pub struct MemoryAP {
    port_number: u8,
}

impl MemoryAP {
    pub fn new(port_number: u8) -> Self {
        Self {
            port_number
        }
    }
}

impl APType for MemoryAP {
    fn get_port_number(&self) -> u8 {
        self.port_number
    }
}

#[derive(Primitive, Clone, Copy)]
pub enum DataSize {
    U8 = 0b000,
    U16 = 0b001,
    U32 = 0b010,
    U64 = 0b011,
    U128 = 0b100,
    U256 = 0b101,
}

impl Default for DataSize {
    fn default() -> Self { DataSize::U32 }
}

/// ADIv5.2 Section 2.6.4
#[allow(non_snake_case)]
#[derive(Default, Clone, Copy)]
pub struct CSW {
    pub(crate) DbgSwEnable:    u8, // 1 bit
    pub(crate) PROT:           u8, // 3 bits
    pub(crate) CACHE:          u8, // 4 bits
    pub(crate) SPIDEN:         u8, // 1 bit
    pub(crate) _RES0:          u8, // 7 bits
    pub(crate) Type:           u8, // 4 bits
    pub(crate) Mode:           u8, // 4 bits
    pub(crate) TrinProg:       u8, // 1 bit
    pub(crate) DeviceEn:       u8, // 1 bit
    pub(crate) AddrInc:        u8, // 2 bits
    pub(crate) _RES1:          u8, // 1 bit
    pub(crate) SIZE:     DataSize, // 3 bits
}

impl Register for CSW {
    const ADDRESS: u16 = 0x000;
}

impl From<u32> for CSW {
    fn from(value: u32) -> CSW {
        CSW {
            DbgSwEnable:((value >> 31) & 0x01) as u8,
            PROT:       ((value >> 28) & 0x03) as u8,
            CACHE:      ((value >> 24) & 0x04) as u8,
            SPIDEN:     ((value >> 23) & 0x01) as u8,
            _RES0:       0,
            Type:       ((value >> 12) & 0x04) as u8,
            Mode:       ((value >>  8) & 0x04) as u8,
            TrinProg:   ((value >>  7) & 0x01) as u8,
            DeviceEn:   ((value >>  6) & 0x01) as u8,
            AddrInc:    ((value >>  4) & 0x02) as u8,
            _RES1:       0,
            SIZE:   DataSize::from_u8(
                        ((value >> 2) & 0x03) as u8
                    // unwrap() is safe as the chip will only return valid values.
                    // If not it's good to crash for now.
                    ).unwrap(),
        }
    }
}

impl From<CSW> for u32 {
    fn from(value: CSW) -> u32 {
          ((value.DbgSwEnable  as u32) << 31)
        | ((value.PROT         as u32) << 28)
        | ((value.CACHE        as u32) << 24)
        | ((value.SPIDEN       as u32) << 23)
        //  value._RES0
        | ((value.Type         as u32) << 12)
        | ((value.Mode         as u32) << 8)
        | ((value.TrinProg     as u32) << 7)
        | ((value.DeviceEn     as u32) << 6)
        | ((value.AddrInc      as u32) << 4)
        //  value._RES1
        // unwrap() is safe!
        | ((value.SIZE.to_u32().unwrap() as u32) << 2)
    }
}

impl APRegister<MemoryAP> for CSW {
    const APBANKSEL: u8 = 0;
}

/// ADIv5.2 Section 2.6.7
#[derive(Default, Clone, Copy)]
pub struct TAR {
    pub(crate) address: u32, // 32 bits
}

impl Register for TAR {
    const ADDRESS: u16 = 0x004;
}

impl From<u32> for TAR {
    fn from(value: u32) -> TAR {
        TAR {
            address: value,
        }
    }
}

impl From<TAR> for u32 {
    fn from(value: TAR) -> u32 {
        value.address
    }
}

impl APRegister<MemoryAP> for TAR {
    const APBANKSEL: u8 = 0;
}

/// ADIv5.2 Section 2.6.5
#[derive(Default, Clone, Copy)]
pub struct DRW {
    pub(crate) data: u32, // 32 bits
}

impl Register for DRW {
    const ADDRESS: u16 = 0x00C;
}

impl From<u32> for DRW {
    fn from(value: u32) -> DRW {
        DRW {
            data: value,
        }
    }
}

impl From<DRW> for u32 {
    fn from(value: DRW) -> u32 {
        value.data
    }
}

impl APRegister<MemoryAP> for DRW {
    const APBANKSEL: u8 = 0;
}