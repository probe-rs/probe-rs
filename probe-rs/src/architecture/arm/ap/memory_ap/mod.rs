//! Memory access port

#[doc(hidden)]
pub(crate) mod mock;

use super::{APAccess, APRegister, AccessPort, GenericAP, Register};
use crate::DebugProbeError;
use enum_primitive_derive::Primitive;
use num_traits::{FromPrimitive, ToPrimitive};

// Memory AP
//
// The memory AP can be used to access a memory-mapped
// set of debug resouces of the attached system.
define_ap!(MemoryAP);

impl MemoryAP {
    pub fn base_address<A>(&self, interface: &mut A) -> Result<u64, DebugProbeError>
    where
        A: APAccess<MemoryAP, BASE, Error = DebugProbeError>
            + APAccess<MemoryAP, BASE2, Error = DebugProbeError>,
    {
        let base_register = interface.read_ap_register(self.port_number(), BASE::default())?;

        let mut base_address = if BaseaddrFormat::ADIv5 == base_register.Format {
            let base2 = interface.read_ap_register(self.port_number(), BASE2::default())?;

            u64::from(base2.BASEADDR) << 32
        } else {
            0
        };
        base_address |= u64::from(base_register.BASEADDR << 12);

        Ok(base_address)
    }
}

impl From<GenericAP> for MemoryAP {
    fn from(other: GenericAP) -> Self {
        MemoryAP {
            port_number: other.port_number(),
        }
    }
}

#[derive(Debug, Primitive, Clone, Copy, PartialEq)]
pub enum DataSize {
    U8 = 0b000,
    U16 = 0b001,
    U32 = 0b010,
    U64 = 0b011,
    U128 = 0b100,
    U256 = 0b101,
}

impl DataSize {
    pub fn from_bytes(bytes: u8) -> Self {
        if bytes == 1 {
            DataSize::U8
        } else if bytes == 2 {
            DataSize::U16
        } else if bytes == 4 {
            DataSize::U32
        } else if bytes == 8 {
            DataSize::U64
        } else if bytes == 16 {
            DataSize::U128
        } else if bytes == 32 {
            DataSize::U256
        } else {
            DataSize::U32
        }
    }
}

impl Default for DataSize {
    fn default() -> Self {
        DataSize::U32
    }
}

#[derive(Debug, Primitive, Clone, Copy, PartialEq)]
pub enum AddressIncrement {
    Off = 0b00,
    Single = 0b01,
    Packed = 0b10,
}

impl Default for AddressIncrement {
    fn default() -> Self {
        AddressIncrement::Single
    }
}

#[derive(Debug, PartialEq, Primitive, Clone, Copy)]
pub enum BaseaddrFormat {
    Legacy = 0,
    ADIv5 = 1,
}

impl Default for BaseaddrFormat {
    fn default() -> Self {
        BaseaddrFormat::Legacy
    }
}

#[derive(Debug, Primitive, Clone, Copy, PartialEq)]
pub enum DebugEntryState {
    NotPresent = 0,
    Present = 1,
}

impl Default for DebugEntryState {
    fn default() -> Self {
        DebugEntryState::NotPresent
    }
}

define_ap_register!(
    /// Base register
    MemoryAP,
    BASE,
    0xF8,
    [
        (BASEADDR: u32),
        (_RES0: u8),
        (Format: BaseaddrFormat),
        (present: bool),
    ],
    value,
    BASE {
        BASEADDR: (value & 0xFFFF_F000) >> 12,
        _RES0: 0,
        Format: match ((value >> 1) & 0x01) as u8 {
            0 => BaseaddrFormat::Legacy,
            1 => BaseaddrFormat::ADIv5,
            _ => panic!("This is a bug. Please report it."),
        },
        present: match (value & 0x01) as u8 {
            0 => false,
            1 => true,
            _ => panic!("This is a bug. Please report it."),
        },
    },
    (value.BASEADDR << 12)
    // _RES0
    | (u32::from(value.Format as u8   ) << 1)
    | (if value.present { 1 } else { 0 })
);

define_ap_register!(
    /// Base register
    MemoryAP,
    BASE2,
    0xF0,
    [(BASEADDR: u32),],
    value,
    BASE2 { BASEADDR: value },
    value.BASEADDR
);

define_ap_register!(
    /// Banked Data 0 register
    MemoryAP,
    BD0,
    0x10,
    [(data: u32),],
    value,
    BD0 { data: value },
    value.data
);

define_ap_register!(
    /// Banked Data 1 register
    MemoryAP,
    BD1,
    0x14,
    [(data: u32),],
    value,
    BD1 { data: value },
    value.data
);

define_ap_register!(
    /// Banked Data 2 register
    MemoryAP,
    BD2,
    0x18,
    [(data: u32),],
    value,
    BD2 { data: value },
    value.data
);

define_ap_register!(
    /// Banked Data 3 register
    MemoryAP,
    BD3,
    0x1C,
    [(data: u32),],
    value,
    BD3 { data: value },
    value.data
);

define_ap_register!(
    /// Configuration register
    ///
    /// The configuration register (CFG) is used to determine
    /// which extensions are included in the memory AP.
    MemoryAP,
    CFG,
    0xF4,
    [(LD: u8), (LA: u8), (BE: u8),],
    value,
    CFG {
        LD: ((value >> 2) & 0x01) as u8,
        LA: ((value >> 1) & 0x01) as u8,
        BE: (value & 0x01) as u8,
    },
    u32::from((value.LD << 2) | (value.LA << 1) | value.BE)
);

define_ap_register!(
    /// Control and Status Word register
    ///
    /// The control and status word register (CSW) is used
    /// to configure memory access through the memory AP.
    MemoryAP,
    CSW,
    0x00,
    [
        (DbgSwEnable: u8),           // 1 bit
        (PROT: u8),                  // 3 bits
        (CACHE: u8),                 // 4 bits
        (SPIDEN: u8),                // 1 bit
        (_RES0: u8),                 // 7 bits
        (Type: u8),                  // 4 bits
        (Mode: u8),                  // 4 bits
        (TrinProg: u8),              // 1 bit
        (DeviceEn: u8),              // 1 bit
        (AddrInc: AddressIncrement), // 2 bits
        (_RES1: u8),                 // 1 bit
        (SIZE: DataSize),            // 3 bits
    ],
    value,
    CSW {
        DbgSwEnable: ((value >> 31) & 0x01) as u8,
        PROT: ((value >> 28) & 0x07) as u8,
        CACHE: ((value >> 24) & 0x0F) as u8,
        SPIDEN: ((value >> 23) & 0x01) as u8,
        _RES0: 0,
        Type: ((value >> 12) & 0x0F) as u8,
        Mode: ((value >> 8) & 0x0F) as u8,
        TrinProg: ((value >> 7) & 0x01) as u8,
        DeviceEn: ((value >> 6) & 0x01) as u8,
        AddrInc: AddressIncrement::from_u8(((value >> 4) & 0x03) as u8).unwrap(),
        _RES1: 0,
        // unwrap() is safe as the chip will only return valid values.
        // If not it's good to crash for now.
        SIZE: DataSize::from_u8((value & 0x07) as u8).unwrap(),
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
    | (u32::from(value.AddrInc as u8) <<  4)
    //  value._RES1
    // unwrap() is safe!
    | value.SIZE.to_u32().unwrap()
);

define_ap_register!(
    /// Data Read/Write register
    ///
    /// The data read/write register (DRW) can be used to read
    /// or write from the memory attached to the memory access point.
    ///
    /// A write to the *DRW* register is translated to a memory write
    /// to the address specified in the TAR register.
    ///
    /// A read from the *DRW* register is translated to a memory read
    /// from the address specified in the TAR register.
    MemoryAP,
    DRW,
    0x0C,
    [(data: u32),],
    value,
    DRW { data: value },
    value.data
);

define_ap_register!(
    /// Memory Barrier Transfer register
    ///
    /// The memory barrier transfer register (MBT) can
    /// be written to generate a barrier operation on the
    /// bus connected to the AP.
    ///
    /// Writes to this register only have an effect if
    /// the *Barrier Operations Extension* is implemented
    /// by the AP.
    MemoryAP,
    MBT,
    0x20,
    [(data: u32)],
    value,
    MBT { data: value },
    value.data
);

define_ap_register!(
    /// Transfer Address Register
    ///
    /// The transfer address register (TAR) holds the memory
    /// address which will be accessed through a read or
    /// write of the DRW register.
    MemoryAP,
    TAR,
    0x04,
    [(address: u32),],
    value,
    TAR { address: value },
    value.address
);
