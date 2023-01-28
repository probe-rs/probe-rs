//! Memory access port

#[doc(hidden)]
pub(crate) mod mock;

use super::{AccessPort, ApAccess, ApRegister, GenericAp, Register};
use crate::architecture::arm::{communication_interface::RegisterParseError, ApAddress, ArmError};
use enum_primitive_derive::Primitive;
use num_traits::{FromPrimitive, ToPrimitive};

define_ap!(
    /// Memory AP
    ///
    /// The memory AP can be used to access a memory-mapped
    /// set of debug resouces of the attached system.
    MemoryAp
);

impl MemoryAp {
    /// The base address of this AP which is used to then access all relative control registers.
    pub fn base_address<A>(&self, interface: &mut A) -> Result<u64, ArmError>
    where
        A: ApAccess,
    {
        let base_register: BASE = interface.read_ap_register(*self)?;

        let mut base_address = if BaseaddrFormat::ADIv5 == base_register.Format {
            let base2: BASE2 = interface.read_ap_register(*self)?;

            u64::from(base2.BASEADDR) << 32
        } else {
            0
        };
        base_address |= u64::from(base_register.BASEADDR << 12);

        Ok(base_address)
    }
}

impl From<GenericAp> for MemoryAp {
    fn from(other: GenericAp) -> Self {
        MemoryAp {
            address: other.ap_address(),
        }
    }
}

/// The unit of data that is transferred in one transfer via the DRW commands.
///
/// This can be configured with the CSW command.
///
/// ALL MCUs support `U32`. All other transfer sizes are optionally implemented.
#[derive(Debug, Primitive, Clone, Copy, PartialEq, Eq)]
pub enum DataSize {
    /// 1 byte transfers are supported.
    U8 = 0b000,
    /// 2 byte transfers are supported.
    U16 = 0b001,
    /// 4 byte transfers are supported.
    U32 = 0b010,
    /// 8 byte transfers are supported.
    U64 = 0b011,
    /// 16 byte transfers are supported.
    U128 = 0b100,
    /// 32 byte transfers are supported.
    U256 = 0b101,
}

impl DataSize {
    /// Create a new `DataSize` from a number of bytes.
    /// Defaults to 4 bytes if the given number of bytes is not available. See [`DataSize`] for available data sizes.
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

/// The increment to the TAR that is performed after each DRW read or write.
///
/// This can be used to avoid successive TAR transfers for writes of consecutive addresses.
/// This will effectively save half the bandwidth!
///
/// Can be configured in the CSW.
#[derive(Debug, Primitive, Clone, Copy, PartialEq, Eq)]
pub enum AddressIncrement {
    /// No increments are happening after the DRW access. TAR always stays the same.
    /// Always supported.
    Off = 0b00,
    /// Increments the TAR by the size of the access after each DRW access.
    /// Always supported.
    Single = 0b01,
    /// Enables packed access to the DRW (see C2.2.7).
    /// Only available if sub-word access is supported by the core.
    Packed = 0b10,
}

impl Default for AddressIncrement {
    fn default() -> Self {
        AddressIncrement::Single
    }
}

/// The format of the BASE register (see C2.6.1).
#[derive(Debug, PartialEq, Eq, Primitive, Clone, Copy)]
pub enum BaseaddrFormat {
    /// The legacy format of very old cores. Very little cores use this.
    Legacy = 0,
    /// The format all newer MCUs use.
    ADIv5 = 1,
}

impl Default for BaseaddrFormat {
    fn default() -> Self {
        BaseaddrFormat::Legacy
    }
}

#[derive(Debug, Primitive, Clone, Copy, PartialEq, Eq)]
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
    type: MemoryAp,
    /// Base register
    name: BASE,
    address: 0xF8,
    fields: [
        /// The base address of this access point.
        BASEADDR: u32,
        /// Reserved.
        _RES0: u8,
        /// The base address format of this access point.
        Format: BaseaddrFormat,
        /// Does this access point exists?
        /// This field can be used to detect access points by iterating over all possible ones until one is found which has `exists == false`.
        present: bool,
    ],
    from: value => Ok(BASE {
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
    }),
   to: value =>
        (value.BASEADDR << 12)
        // _RES0
        | (u32::from(value.Format as u8) << 1)
        | u32::from(value.present)
);

define_ap_register!(
    type: MemoryAp,
    /// Base register
    name: BASE2,
    address: 0xF0,
    fields: [
        /// The second part of the base address of this access point if required.
        BASEADDR: u32
    ],
    from: value => Ok(BASE2 { BASEADDR: value }),
    to: value => value.BASEADDR
);

define_ap_register!(
    type: MemoryAp,
    /// Banked Data 0 register
    name: BD0,
    address: 0x10,
    fields: [
        /// The data held in this bank.
        data: u32,
    ],
    from: value => Ok(BD0 { data: value }),
    to: value => value.data
);

define_ap_register!(
    type: MemoryAp,
    /// Banked Data 1 register
    name: BD1,
    address: 0x14,
    fields: [
        /// The data held in this bank.
        data: u32,
    ],
    from: value => Ok(BD1 { data: value }),
    to: value => value.data
);

define_ap_register!(
    type: MemoryAp,
    /// Banked Data 2 register
    name: BD2,
    address: 0x18,
    fields: [
        /// The data held in this bank.
        data: u32,
    ],
    from: value => Ok(BD2 { data: value }),
    to: value => value.data
);

define_ap_register!(
    type: MemoryAp,
    /// Banked Data 3 register
    name: BD3,
    address: 0x1C,
    fields: [
        /// The data held in this bank.
        data: u32,
    ],
    from: value => Ok(BD3 { data: value }),
    to: value => value.data
);

define_ap_register!(
    type: MemoryAp,
    /// Configuration register
    ///
    /// The configuration register (CFG) is used to determine
    /// which extensions are included in the memory AP.
    name: CFG,
    address: 0xF4,
    fields: [
        /// Specifies whether this access port includes the large data extension (access larger than 32 bits).
        LD: u8,
        /// Specifies whether this access port includes the large address extension (64 bit addressing).
        LA: u8,
        /// Specifies whether this architecture uses big endian. Must always be zero for modern chips as the ADI v5.2 deprecates big endian.
        BE: u8,
    ],
    from: value => Ok(CFG {
        LD: ((value >> 2) & 0x01) as u8,
        LA: ((value >> 1) & 0x01) as u8,
        BE: (value & 0x01) as u8,
    }),
    to: value => u32::from((value.LD << 2) | (value.LA << 1) | value.BE)
);

define_ap_register!(
    type: MemoryAp,
    /// Control and Status Word register
    ///
    /// The control and status word register (CSW) is used
    /// to configure memory access through the memory AP.
    name: CSW,
    address: 0x00,
    fields: [
        /// Is debug software access enabled.
        DbgSwEnable: u8,           // 1 bit
        /// Specifies whether HNONSEC is enabled.
        HNONSEC: u8,               // 1 bit
        /// Prot
        PROT: u8,                  // 2 bits
        /// Cache
        CACHE: u8,                 // 4 bits
        /// Secure Debug Enabled. This field has one of the following values:
        /// - `0b0` Secure access is disabled.
        /// - `0b1` Secure access is enabled.
        /// This field is optional, and read-only. If not implemented, the bit is RES0.
        /// If CSW.DEVICEEN is 0b0, SDEVICEEN is ignored and the effective value of SDEVICEEN is 0b1.
        /// For more information, see Enabling access to the connected debug device or memory system on page C2-154.
        /// Note
        /// In ADIv5 and older versions of the architecture, the CSW.SPIDEN field is in the same bit position as CSW.SDeviceEn, and has the same meaning. From ADIv6, the name SDeviceEn is used to avoid confusion between this field and the SPIDEN signal on the authentication interface.
        SPIDEN: u8,                // 1 bit
        /// Reserved.
        _RES0: u8,                 // 7 bits
        /// `1` if memory tagging access is enabled.
        MTE: u8,                   // 1 bits
        /// Memory tagging type. Implementation defined.
        Type: u8,                  // 3 bits
        /// Mode of operation. Is set to `0b0000` normally.
        Mode: u8,                  // 4 bits
        /// A transfer is in progress.
        /// Can be used to poll whether an aborted transaction has completed.
        /// Read only.
        TrinProg: u8,              // 1 bit
        /// `1` if transactions can be issued through this access port at the moment.
        /// Read only.
        DeviceEn: u8,              // 1 bit
        /// The address increment on DRW access.
        AddrInc: AddressIncrement, // 2 bits
        /// Reserved
        _RES1: u8,                 // 1 bit
        /// The access size of this memory AP.
        SIZE: DataSize,            // 3 bits
    ],
    from: value => Ok(CSW {
        DbgSwEnable: ((value >> 31) & 0x01) as u8,
        HNONSEC: ((value >> 30) & 0x01) as u8,
        PROT: ((value >> 28) & 0x03) as u8,
        CACHE: ((value >> 24) & 0x0F) as u8,
        SPIDEN: ((value >> 23) & 0x01) as u8,
        _RES0: 0,
        MTE: ((value >> 15) & 0x01) as u8,
        Type: ((value >> 12) & 0x07) as u8,
        Mode: ((value >> 8) & 0x0F) as u8,
        TrinProg: ((value >> 7) & 0x01) as u8,
        DeviceEn: ((value >> 6) & 0x01) as u8,
        AddrInc: AddressIncrement::from_u8(((value >> 4) & 0x03) as u8).ok_or_else(|| RegisterParseError::new("CSW", value))?,
        _RES1: 0,
        SIZE: DataSize::from_u8((value & 0x07) as u8).ok_or_else(|| RegisterParseError::new("CSW", value))?,
    }),
    to: value => (u32::from(value.DbgSwEnable) << 31)
    | (u32::from(value.HNONSEC    ) << 30)
    | (u32::from(value.PROT       ) << 28)
    | (u32::from(value.CACHE      ) << 24)
    | (u32::from(value.SPIDEN     ) << 23)
    | (u32::from(value.MTE        ) << 15)
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

impl CSW {
    /// Creates a new CSW content with default values and a configurable [`DataSize`].
    /// See in code documentation for more info.
    ///
    /// The CSW Register is set for an AMBA AHB Acccess, according to
    /// the ARM Debug Interface Architecture Specification.
    ///
    /// The PROT bits are set as follows:
    ///
    /// ```text
    /// HNONSEC[30]          = 1  - Should be One, if not supported.
    /// MasterType, bit [29] = 1  - Access as default AHB Master
    /// HPROT[4]             = 0  - Non-allocating access
    /// ```
    ///
    /// The CACHE bits are set for the following AHB access:
    ///
    /// ```text
    /// HPROT[0] == 1   - data           access
    /// HPROT[1] == 1   - privileged     access
    /// HPROT[2] == 0   - non-cacheable  access
    /// HPROT[3] == 0   - non-bufferable access
    /// ```
    pub fn new(data_size: DataSize) -> Self {
        CSW {
            DbgSwEnable: 0b1,
            HNONSEC: 0b1,
            PROT: 0b110,
            CACHE: 0b11,
            AddrInc: AddressIncrement::Single,
            SIZE: data_size,
            ..Default::default()
        }
    }
}

define_ap_register!(
    type: MemoryAp,
    /// Data Read/Write register
    ///
    /// The data read/write register (DRW) can be used to read
    /// or write from the memory attached to the memory access point.
    ///
    /// A write to the *DRW* register is translated to a memory write
    /// to the address specified in the TAR register.
    ///
    /// A read from the *DRW* register is translated to a memory read
    name: DRW,
    address: 0x0C,
    fields: [
        /// The data held in the DRW corresponding to the address held in TAR.
        data: u32,
    ],
    from: value => Ok(DRW { data: value }),
    to: value => value.data
);

define_ap_register!(
    type: MemoryAp,
    /// Memory Barrier Transfer register
    ///
    /// The memory barrier transfer register (MBT) can
    /// be written to generate a barrier operation on the
    /// bus connected to the AP.
    ///
    /// Writes to this register only have an effect if
    /// the *Barrier Operations Extension* is implemented
    name: MBT,
    address: 0x20,
    fields: [
        /// This value is implementation defined and the ADIv5.2 spec does not explain what it does for targets with the Barrier Operations Extension implemented.
        data: u32,
    ],
    from: value => Ok(MBT { data: value }),
    to: value => value.data
);

define_ap_register!(
    type: MemoryAp,
    /// Transfer Address Register
    ///
    /// The transfer address register (TAR) holds the memory
    /// address which will be accessed through a read or
    /// write of the DRW register.
    name: TAR,
    address: 0x04,
    fields: [
        /// The register address to be used for the next access to DRW.
        address: u32,
    ],
    from: value => Ok(TAR { address: value }),
    to: value => value.address
);

define_ap_register!(
    type: MemoryAp,
    /// Transfer Address Register - upper word
    ///
    /// The transfer address register (TAR) holds the memory
    /// address which will be accessed through a read or
    /// write of the DRW register.
    name: TAR2,
    address: 0x08,
    fields: [
        /// The uppper 32-bits of the register address to be used for the next access to DRW.
        address: u32,
    ],
    from: value => Ok(TAR2 { address: value }),
    to: value => value.address
);
