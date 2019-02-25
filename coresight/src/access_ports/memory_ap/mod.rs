pub mod mock;

use crate::common::Register;
use num_traits::{
    FromPrimitive,
    ToPrimitive,
};
use enum_primitive_derive::Primitive;

use crate::access_ports::APRegister;
use crate::ap_access::AccessPort;

define_ap!(MemoryAP);

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