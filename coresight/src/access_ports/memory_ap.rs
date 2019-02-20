use num_traits::{
    FromPrimitive,
    ToPrimitive,
};
use enum_primitive_derive::Primitive;

use crate::access_ports::APRegister;
use crate::access_ports::APValue;
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

#[derive(Clone, Copy)]
pub enum MemoryAPRegister {
    CSW = 0x000,
    TAR0 = 0x004,
    DRW = 0x00C,
}

impl APRegister<MemoryAPValue> for MemoryAPRegister {
    fn to_u16(&self) -> u16 {
        *self as u16
    }

    fn get_value(&self, value: u32) -> MemoryAPValue {
        use MemoryAPRegister as R;
        use MemoryAPValue as V;
        match self {
            R::CSW => V::CSW(Default::default()).from_u32(value),
            R::TAR0 => V::TAR0(Default::default()).from_u32(value),
            R::DRW => V::DRW(Default::default()).from_u32(value),
        }
    }

    fn get_apbanksel(&self) -> u8 {
        use MemoryAPRegister as R;
        match self {
            R::CSW => 0,
            R::TAR0 => 0,
            R::DRW => 0,
        }
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

/// ADIv5.2 Section 2.6.7
#[derive(Default, Clone, Copy)]
pub struct TAR {
    pub(crate) address: u32, // 32 bits
}

/// ADIv5.2 Section 2.6.5
#[derive(Default, Clone, Copy)]
pub struct DRW {
    pub(crate) data: u32, // 32 bits
}

pub enum MemoryAPValue {
    CSW(CSW),
    TAR0(TAR),
    DRW(DRW)
}

impl APValue for MemoryAPValue {
    fn from_u32(self, value: u32) -> Self {
        use MemoryAPValue as V;
        match self {
            V::CSW(_) => V::CSW(CSW {
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
            }),
            V::TAR0(_) => V::TAR0(TAR {
                address: value,
            }),
            V::DRW(_) => V::DRW(DRW {
                data: value,
            }),
        }
    }

    fn to_u32(&self) -> u32 {
        use MemoryAPValue as V;
        match self {
            V::CSW(v) =>
                  ((v.DbgSwEnable  as u32) << 31)
                | ((v.PROT         as u32) << 28)
                | ((v.CACHE        as u32) << 24)
                | ((v.SPIDEN       as u32) << 23)
                //  v._RES0
                | ((v.Type         as u32) << 12)
                | ((v.Mode         as u32) << 8)
                | ((v.TrinProg     as u32) << 7)
                | ((v.DeviceEn     as u32) << 6)
                | ((v.AddrInc      as u32) << 4)
                //  v._RES1
                | ((v.SIZE
                    .to_u32()
                    // unwrap() is safe!
                    .unwrap()      as u32) << 2),
            V::TAR0(v) => v.address,
            V::DRW(v) => v.data,
        }
    }
}

// pub trait Register<V> {
//     const ADDRESS: u16;
//     const APSEL: u8;
//     const AP: u8;
// }