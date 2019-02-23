pub mod mock;

use crate::common::Register;
use num_traits::{
    FromPrimitive,
    ToPrimitive,
};
use enum_primitive_derive::Primitive;

use crate::access_ports::APRegister;
use crate::access_ports::APType;

#[derive(Clone, Copy)]
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

#[derive(Debug, Primitive, Clone, Copy)]
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

macro_rules! define_ap_register {
    ($port_type:ident, $name:ident, $address:expr, $apbanksel:expr, [$(($field:ident: $type:ty)$(,)?)*], $param:ident, $from:expr, $to:expr) => {
        #[allow(non_snake_case)]
        #[derive(Debug, Default, Clone, Copy)]
        pub struct $name {
            $(pub(crate) $field: $type,)*
        }

        impl Register for $name {
            const ADDRESS: u16 = $address;
        }

        impl From<u32> for $name {
            fn from($param: u32) -> $name {
                $from
            }
        }

        impl From<$name> for u32 {
            fn from($param: $name) -> u32 {
                $to
            }
        }

        impl APRegister<$port_type> for $name {
            const APBANKSEL: u8 = $apbanksel;
        }
    }
}

define_ap_register!(MemoryAP, CSW, 0x000, 0, [
        (DbgSwEnable:    u8), // 1 bit
        (PROT:           u8), // 3 bits
        (CACHE:          u8), // 4 bits
        (SPIDEN:         u8), // 1 bit
        (_RES0:          u8), // 7 bits
        (Type:           u8), // 4 bits
        (Mode:           u8), // 4 bits
        (TrinProg:       u8), // 1 bit
        (DeviceEn:       u8), // 1 bit
        (AddrInc:        u8), // 2 bits
        (_RES1:          u8), // 1 bit
        (SIZE:     DataSize), // 3 bits
    ],
    value,
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
                // unwrap() is safe as the chip will only return valid values.
                // If not it's good to crash for now.
        SIZE:   DataSize::from_u8((value & 0x03) as u8).unwrap(),
    },
      (u32::from(value.DbgSwEnable) << 31)
    | (u32::from(value.PROT       ) << 28)
    | (u32::from(value.CACHE      ) << 24)
    | (u32::from(value.SPIDEN     ) << 23)
    //  value._RES0
    | (u32::from(value.Type       ) << 12)
    | (u32::from(value.Mode       ) <<  8)
    | (u32::from(value.TrinProg   ) <<  7)
    | (u32::from(value.DeviceEn   ) <<  6)
    | (u32::from(value.AddrInc    ) <<  4)
    //  value._RES1
    // unwrap() is safe!
    | value.SIZE.to_u32().unwrap()
);

define_ap_register!(MemoryAP, TAR, 0x004, 0, [
        (address: u32),
    ],
    value,
    TAR {
        address: value
    },
    value.address
);

define_ap_register!(MemoryAP, DRW, 0x00C, 0, [
        (data: u32),
    ],
    value,
    DRW {
        data: value
    },
    value.data
);