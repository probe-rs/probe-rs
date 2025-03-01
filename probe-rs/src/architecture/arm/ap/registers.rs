use crate::architecture::arm::{
    RegisterParseError,
    ap::{AddressIncrement, ApClass, ApType, BaseAddrFormat, DataSize},
};

/// Defines a new typed access port register for a specific access port.
/// Takes
/// - type: The type of the port.
/// - name: The name of the constructed type for the register. Also accepts a doc comment to be added to the type.
/// - address: The address relative to the base address of the access port.
/// - fields: A list of fields of the register type.
/// - from: a closure to transform from an `u32` to the typed register.
/// - to: A closure to transform from they typed register to an `u32`.
macro_rules! define_ap_register {
    (
        $(#[$outer:meta])*
        name: $name:ident,
        address: $address_v1:expr,
        fields: [$($(#[$inner:meta])*$field:ident: $type:ty$(,)?)*],
        from: $from_param:ident => $from:expr,
        to: $to_param:ident => $to:expr
    )
    => {
        $(#[$outer])*
        #[allow(non_snake_case)]
        #[allow(clippy::upper_case_acronyms)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name {
            $($(#[$inner])*pub $field: $type,)*
        }

        impl $crate::architecture::arm::ap::ApRegister for $name {
            const NAME: &'static str = stringify!($name);

            // APv1 registers only use the lower 8-bits of the address, so they ignore the static
            // offset used by APv2 registers at the DAP access layer.
            const ADDRESS: u64 = 0xD00 | $address_v1;
        }

        impl TryFrom<u32> for $name {
            type Error = $crate::architecture::arm::RegisterParseError;

            fn try_from($from_param: u32) -> Result<$name, Self::Error> {
                $from
            }
        }

        impl From<$name> for u32 {
            fn from($to_param: $name) -> u32 {
                $to
            }
        }
    }
}

pub(crate) use define_ap_register;

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
        /// If CSW.DeviceEn is 0b0, the value is ignored and the effective value is 0b1.
        /// For more information, see `Enabling access to the connected debug device or memory system`
        /// on page C2-177.
        ///
        /// Note:
        /// In ADIv5 and older versions of the architecture, the CSW.SPIDEN field is in the same bit
        /// position as CSW.SDeviceEn, and has the same meaning. From ADIv6, the name SDeviceEn is
        /// used to avoid confusion between this field and the SPIDEN signal on the authentication
        /// interface.
        SDeviceEn: bool,                // 1 bit
        /// Realm and root access status.
        ///
        /// # Note
        /// This field is RES0 for ADIv5.
        ///
        /// When CFG.RME == 0b1, the defined values of this field are:
        /// * 0b00 - Realm and Root accesses are disabled
        /// * 0b01 - Realm access is enabled. Root access is disabled.
        /// * 0b01 - Realm access is enabled. Root access is enabled.
        ///
        /// This field is read-only.
        RMEEN: u8, //2 bits
        /// Reserved.
        _RES0: u8,                 // 7 bits

        /// Errors prevent future memory accesses.
        ///
        /// # Note
        /// This field is RES0 for ADIv5.
        ///
        /// Value:
        /// - 0b0 - Memory access errors do not prevent future memory accesses.
        /// - 0b1 - Memory access errors prevent future memory accesses.
        ///
        /// CFG.ERR indicates whether this field is implemented.
        ERRSTOP: bool,

        /// Errors are not passed upstream.
        ///
        /// # Note
        /// This field is RES0 for ADIv5.
        ///
        /// Value:
        /// - 0b0 - Errors are passed upstream.
        /// - 0b1 - Errors are not passed upstream.
        ///
        /// CFG.ERR indicates whether this field is implemented.
        ERRNPASS: bool,
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
        SDeviceEn: ((value >> 23) & 0x01) != 0,
        RMEEN: ((value >> 21) & 0x3) as u8,
        _RES0: ((value >> 18) & 0x07) as u8,
        ERRSTOP: ((value >> 17) & 0b1) != 0,
        ERRNPASS: ((value >> 16) & 0b1) != 0,
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
    | (u32::from(value.SDeviceEn    ) << 23)
    | (u32::from(value.RMEEN        ) << 21)
    | (u32::from(value._RES0        ) << 18)
    | (u32::from(value.ERRSTOP as u8) << 17)
    | (u32::from(value.ERRNPASS as u8) << 16)
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

define_ap_register!(
    /// Identification Register
    ///
    /// The identification register is used to identify
    /// an AP.
    ///
    /// It has to be present on every AP.
    name: IDR,
    address: 0x0FC,
    fields: [
        /// The revision of this access point.
        REVISION: u8,
        /// The JEP106 code of the designer of this access point.
        DESIGNER: jep106::JEP106Code,
        /// The class of this access point.
        CLASS: ApClass,
        #[doc(hidden)]
        _RES0: u8,
        /// The variant of this access port.
        VARIANT: u8,
        /// The type of this access port.
        TYPE: ApType,
    ],
    from: value => Ok(IDR {
        REVISION: ((value >> 28) & 0x0F) as u8,
        DESIGNER: {
            let designer = ((value >> 17) & 0x7FF) as u16;
            let cc = (designer >> 7) as u8;
            let id = (designer & 0x7f) as u8;

            jep106::JEP106Code::new(cc, id)
        },
        CLASS: ApClass::from_u8(((value >> 13) & 0x0F) as u8).ok_or_else(|| RegisterParseError::new("IDR", value))?,
        _RES0: 0,
        VARIANT: ((value >> 4) & 0x0F) as u8,
        TYPE: ApType::from_u8((value & 0x0F) as u8).ok_or_else(|| RegisterParseError::new("IDR", value))?
    }),
    to: value => (u32::from(value.REVISION) << 28)
        | (((u32::from(value.DESIGNER.cc) << 7) | u32::from(value.DESIGNER.id)) << 17)
        | ((value.CLASS as u32) << 13)
        | (u32::from(value.VARIANT) << 4)
        | (value.TYPE as u32)
);
