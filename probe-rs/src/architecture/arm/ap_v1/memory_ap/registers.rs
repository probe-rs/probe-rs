use crate::architecture::arm::communication_interface::RegisterParseError;

/// The unit of data that is transferred in one transfer via the DRW commands.
///
/// This can be configured with the CSW command.
///
/// ALL MCUs support `U32`. All other transfer sizes are optionally implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataSize {
    /// 1 byte transfers are supported.
    U8 = 0b000,
    /// 2 byte transfers are supported.
    U16 = 0b001,
    /// 4 byte transfers are supported.
    #[default]
    U32 = 0b010,
    /// 8 byte transfers are supported.
    U64 = 0b011,
    /// 16 byte transfers are supported.
    U128 = 0b100,
    /// 32 byte transfers are supported.
    U256 = 0b101,
}

impl DataSize {
    pub fn to_byte_count(self) -> usize {
        match self {
            DataSize::U8 => 1,
            DataSize::U16 => 2,
            DataSize::U32 => 4,
            DataSize::U64 => 8,
            DataSize::U128 => 16,
            DataSize::U256 => 32,
        }
    }
}

/// Invalid data size.
pub struct InvalidDataSizeError;

impl TryFrom<u8> for DataSize {
    type Error = InvalidDataSizeError;
    fn try_from(value: u8) -> Result<Self, InvalidDataSizeError> {
        match value {
            0b000 => Ok(DataSize::U8),
            0b001 => Ok(DataSize::U16),
            0b010 => Ok(DataSize::U32),
            0b011 => Ok(DataSize::U64),
            0b100 => Ok(DataSize::U128),
            0b101 => Ok(DataSize::U256),
            _ => Err(InvalidDataSizeError),
        }
    }
}

/// The increment to the TAR that is performed after each DRW read or write.
///
/// This can be used to avoid successive TAR transfers for writes of consecutive addresses.
/// This will effectively save half the bandwidth!
///
/// Can be configured in the CSW.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddressIncrement {
    /// No increments are happening after the DRW access. TAR always stays the same.
    /// Always supported.
    Off = 0b00,
    /// Increments the TAR by the size of the access after each DRW access.
    /// Always supported.
    #[default]
    Single = 0b01,
    /// Enables packed access to the DRW (see C2.2.7).
    /// Only available if sub-word access is supported by the core.
    Packed = 0b10,
}

impl AddressIncrement {
    /// Create a new `AddressIncrement` from a u8.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0b00 => Some(AddressIncrement::Off),
            0b01 => Some(AddressIncrement::Single),
            0b10 => Some(AddressIncrement::Packed),
            _ => None,
        }
    }
}

/// The format of the BASE register (see C2.6.1).
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
pub enum BaseAddrFormat {
    /// The legacy format of very old cores. Very little cores use this.
    #[default]
    Legacy = 0,
    /// The format all newer MCUs use.
    ADIv5 = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)] // Present is not used yet.
pub enum DebugEntryState {
    #[default]
    NotPresent = 0,
    Present = 1,
}

define_ap_register!(
    /// Control and Status Word register
    ///
    /// The control and status word register (CSW) is used
    /// to configure memory access through the memory AP.
    name: CSW,
    address: 0x00,
    fields: [
        /// Is debug software access enabled.
        DbgSwEnable: bool,           // 1 bit
        /// Used with the Type field to define the bus access protection protocol.
        ///
        /// This field is implementation defined. See the memory ap specific definition for details.
        Prot: u8,                  // 7 bits
        /// Secure Debug Enabled.
        ///
        /// This field has one of the following values:
        /// - `0b0` Secure access is disabled.
        /// - `0b1` Secure access is enabled.
        /// This field is optional, and read-only. If not implemented, the bit is RES0.
        /// If CSW.DeviceEn is 0b0, SPIDEN is ignored and the effective value of SPIDEN is 0b1.
        /// For more information, see `Enabling access to the connected debug device or memory system`
        /// on page C2-154.
        ///
        /// Note:
        /// In ADIv5 and older versions of the architecture, the CSW.SPIDEN field is in the same bit
        /// position as CSW.SDeviceEn, and has the same meaning. From ADIv6, the name SDeviceEn is
        /// used to avoid confusion between this field and the SPIDEN signal on the authentication
        /// interface.
        SPIDEN: bool,                // 1 bit
        /// Reserved.
        _RES0: u8,                 // 7 bits
        /// `1` if memory tagging access is enabled.
        MTE: bool,                   // 1 bits
        /// Memory tagging type. Implementation defined.
        Type: u8,                  // 3 bits
        /// Mode of operation. Is set to `0b0000` normally.
        Mode: u8,                  // 4 bits
        /// A transfer is in progress.
        /// Can be used to poll whether an aborted transaction has completed.
        /// Read only.
        TrInProg: bool,              // 1 bit
        /// `1` if transactions can be issued through this access port at the moment.
        /// Read only.
        DeviceEn: bool,              // 1 bit
        /// The address increment on DRW access.
        AddrInc: AddressIncrement, // 2 bits
        /// Reserved
        _RES1: u8,                 // 1 bit
        /// The access size of this memory AP.
        SIZE: DataSize,            // 3 bits
    ],
    from: value => Ok(CSW {
        DbgSwEnable: ((value >> 31) & 0x01) != 0,
        Prot: ((value >> 24) & 0x7F) as u8,
        SPIDEN: ((value >> 23) & 0x01) != 0,
        _RES0: ((value >> 16) & 0x7F) as u8,
        MTE: ((value >> 15) & 0x01) != 0,
        Type: ((value >> 12) & 0x07) as u8,
        Mode: ((value >> 8) & 0x0F) as u8,
        TrInProg: ((value >> 7) & 0x01) != 0,
        DeviceEn: ((value >> 6) & 0x01) != 0,
        AddrInc: AddressIncrement::from_u8(((value >> 4) & 0x03) as u8).ok_or_else(|| RegisterParseError::new("CSW", value))?,
        _RES1: ((value >> 3) & 1) as u8,
        SIZE: DataSize::try_from((value & 0x07) as u8).map_err(|_| RegisterParseError::new("CSW", value))?,
    }),
    to: value => (u32::from(value.DbgSwEnable) << 31)
    | (u32::from(value.Prot         ) << 24)
    | (u32::from(value.SPIDEN       ) << 23)
    | (u32::from(value._RES0        ) << 16)
    | (u32::from(value.MTE          ) << 15)
    | (u32::from(value.Type         ) << 12)
    | (u32::from(value.Mode         ) <<  8)
    | (u32::from(value.TrInProg     ) <<  7)
    | (u32::from(value.DeviceEn     ) <<  6)
    | (u32::from(value.AddrInc as u8) << 4)
    | (u32::from(value._RES1        ) <<  1)
    | (value.SIZE as u32)
);

define_ap_register!(
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
    /// Transfer Address Register - upper word
    ///
    /// The transfer address register (TAR) holds the memory
    /// address which will be accessed through a read or
    /// write of the DRW register.
    name: TAR2,
    address: 0x08,
    fields: [
        /// The upper 32-bits of the register address to be used for the next access to DRW.
        address: u32,
    ],
    from: value => Ok(TAR2 { address: value }),
    to: value => value.address
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
    /// Configuration register
    ///
    /// The configuration register (CFG) is used to determine
    /// which extensions are included in the memory AP.
    name: CFG,
    address: 0xF4,
    fields: [
        /// Specifies whether this access port includes the large data extension (access larger than 32 bits).
        LD: bool,
        /// Specifies whether this access port includes the large address extension (64 bit addressing).
        LA: bool,
        /// Specifies whether this architecture uses big endian. Must always be zero for modern chips as the ADI v5.2 deprecates big endian.
        BE: bool,
    ],
    from: value => Ok(CFG {
        LD: ((value >> 2) & 0x01) != 0,
        LA: ((value >> 1) & 0x01) != 0,
        BE: (value & 0x01) != 0,
    }),
    to: value => ((value.LD as u32) << 2) | ((value.LA as u32) << 1) | (value.BE as u32)
);

define_ap_register!(
    /// Base register
    name: BASE,
    address: 0xF8,
    fields: [
        /// The base address of this access point.
        BASEADDR: u32,
        /// Reserved.
        _RES0: u8,
        /// The base address format of this access point.
        Format: BaseAddrFormat,
        /// Does this access point exists?
        /// This field can be used to detect access points by iterating over all possible ones until one is found which has `exists == false`.
        present: bool,
    ],
    from: value => Ok(BASE {
        BASEADDR: (value & 0xFFFF_F000) >> 12,
        _RES0: 0,
        Format: match ((value >> 1) & 0x01) as u8 {
            0 => BaseAddrFormat::Legacy,
            1 => BaseAddrFormat::ADIv5,
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
