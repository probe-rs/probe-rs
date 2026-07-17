// bitfield! generates accessor methods named after the register fields, which
// use the ADI register casing (e.g. `DbgSwEnable`, `SIZE`) rather than snake_case.
#![allow(non_snake_case)]

use crate::architecture::arm::ap::{AddressIncrement, ApClass, ApType, BaseAddrFormat, DataSize};

/// Defines a new typed access port register for a specific access port.
///
/// The register is a thin `bitfield!` newtype over the raw `u32`, so the field
/// list is given directly in [`bitfield::bitfield!`] syntax. Bits that are not
/// listed are preserved verbatim on a read/modify/write round-trip.
///
/// Takes
/// - name: The name of the constructed type for the register. Also accepts a doc comment to be added to the type.
/// - address: The address relative to the base address of the access port.
/// - fields: The register fields in `bitfield!` syntax. Enum-backed fields use
///   `pub u8, from into EnumType, getter, setter: msb, lsb;`; an unknown encoding
///   is preserved as the enum's `Unknown(bits)` variant rather than erroring.
macro_rules! define_ap_register {
    (
        $(#[$outer:meta])*
        name: $name:ident,
        address: $address_v1:expr,
        fields: [ $($fields:tt)* ]
        $(,)?
    ) => {
        bitfield::bitfield! {
            $(#[$outer])*
            #[derive(Copy, Clone, PartialEq, Eq)]
            #[allow(clippy::upper_case_acronyms)]
            pub struct $name(u32);
            impl Debug;
            $($fields)*
        }

        impl $name {
            /// Creates the register from its raw value.
            pub const fn from_raw(value: u32) -> Self {
                $name(value)
            }
        }

        impl $crate::architecture::arm::ap::ApRegister for $name {
            const NAME: &'static str = stringify!($name);

            // APv1 registers only use the lower 8-bits of the address, so they ignore the static
            // offset used by APv2 registers at the DAP access layer.
            const ADDRESS: u64 = 0xD00 | $address_v1;
        }

        impl TryFrom<u32> for $name {
            type Error = $crate::architecture::arm::RegisterParseError;

            // Registers parse infallibly: unknown enum encodings are preserved as the
            // enum's `Unknown` variant. `TryFrom` is kept because `ApRegister` requires it.
            fn try_from(value: u32) -> Result<$name, Self::Error> {
                Ok($name(value))
            }
        }

        impl From<$name> for u32 {
            fn from(register: $name) -> u32 {
                register.0
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
        pub DbgSwEnable, set_DbgSwEnable: 31;
        /// Used with the Type field to define the bus access protection protocol.
        ///
        /// This field is implementation defined. See the memory ap specific definition for details.
        pub u8, Prot, set_Prot: 30, 24;
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
        pub SDeviceEn, set_SDeviceEn: 23;
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
        pub u8, RMEEN, set_RMEEN: 22, 21;
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
        pub ERRSTOP, set_ERRSTOP: 17;
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
        pub ERRNPASS, set_ERRNPASS: 16;
        /// `1` if memory tagging access is enabled.
        pub MTE, set_MTE: 15;
        /// Memory tagging type. Implementation defined.
        pub u8, Type, set_Type: 14, 12;
        /// Mode of operation. Is set to `0b0000` normally.
        pub u8, Mode, set_Mode: 11, 8;
        /// A transfer is in progress.
        /// Can be used to poll whether an aborted transaction has completed.
        /// Read only.
        pub TrInProg, set_TrInProg: 7;
        /// `1` if transactions can be issued through this access port at the moment.
        /// Read only.
        pub DeviceEn, set_DeviceEn: 6;
        /// The address increment on DRW access.
        pub u8, from into AddressIncrement, AddrInc, set_AddrInc: 5, 4;
        /// The access size of this memory AP.
        pub u8, from into DataSize, SIZE, set_SIZE: 2, 0;
    ]
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
        pub u32, address, set_address: 31, 0;
    ]
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
        pub u32, address, set_address: 31, 0;
    ]
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
        pub u32, data, set_data: 31, 0;
    ]
);

define_ap_register!(
    /// Banked Data 0 register
    name: BD0,
    address: 0x10,
    fields: [
        /// The data held in this bank.
        pub u32, data, set_data: 31, 0;
    ]
);

define_ap_register!(
    /// Banked Data 1 register
    name: BD1,
    address: 0x14,
    fields: [
        /// The data held in this bank.
        pub u32, data, set_data: 31, 0;
    ]
);

define_ap_register!(
    /// Banked Data 2 register
    name: BD2,
    address: 0x18,
    fields: [
        /// The data held in this bank.
        pub u32, data, set_data: 31, 0;
    ]
);

define_ap_register!(
    /// Banked Data 3 register
    name: BD3,
    address: 0x1C,
    fields: [
        /// The data held in this bank.
        pub u32, data, set_data: 31, 0;
    ]
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
        pub u32, data, set_data: 31, 0;
    ]
);

define_ap_register!(
    /// Base register
    name: BASE2,
    address: 0xF0,
    fields: [
        /// The second part of the base address of this access point if required.
        pub u32, BASEADDR, set_BASEADDR: 31, 0;
    ]
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
        pub LD, set_LD: 2;
        /// Specifies whether this access port includes the large address extension (64 bit addressing).
        pub LA, set_LA: 1;
        /// Specifies whether this architecture uses big endian. Must always be zero for modern chips as the ADI v5.2 deprecates big endian.
        pub BE, set_BE: 0;
    ]
);

define_ap_register!(
    /// Base register
    name: BASE,
    address: 0xF8,
    fields: [
        /// The base address of this access point.
        pub u32, BASEADDR, set_BASEADDR: 31, 12;
        /// The base address format of this access point.
        pub u8, from into BaseAddrFormat, Format, set_Format: 1, 1;
        /// Does this access point exists?
        /// This field can be used to detect access points by iterating over all possible ones until one is found which has `present == false`.
        pub present, set_present: 0;
    ]
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
        pub u8, REVISION, set_REVISION: 31, 28;
        /// The raw JEP106 designer bits. Use [`IDR::DESIGNER`] for the decoded value.
        u16, designer_bits, set_designer_bits: 27, 17;
        /// The class of this access point.
        pub u8, from into ApClass, CLASS, set_CLASS: 16, 13;
        /// The variant of this access port.
        pub u8, VARIANT, set_VARIANT: 7, 4;
        /// The type of this access port.
        pub u8, from into ApType, TYPE, set_TYPE: 3, 0;
    ]
);

impl IDR {
    /// The JEP106 code of the designer of this access point.
    pub fn DESIGNER(&self) -> jep106::JEP106Code {
        let designer = self.designer_bits();
        let cc = (designer >> 7) as u8;
        let id = (designer & 0x7f) as u8;
        jep106::JEP106Code::new(cc, id)
    }

    /// Sets the JEP106 code of the designer of this access point.
    pub fn set_DESIGNER(&mut self, designer: jep106::JEP106Code) {
        self.set_designer_bits((u16::from(designer.cc) << 7) | u16::from(designer.id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::architecture::arm::ap::{AddressIncrement, ApClass, DataSize};

    #[test]
    fn csw_field_positions_and_roundtrip() {
        let mut csw = CSW::try_from(0).unwrap();
        csw.set_DbgSwEnable(true); // bit 31
        csw.set_AddrInc(AddressIncrement::Packed); // bits 5:4 = 0b10
        csw.set_SIZE(DataSize::U256); // bits 2:0 = 0b101

        let raw: u32 = csw.into();
        assert_eq!(raw & (1 << 31), 1 << 31, "DbgSwEnable must be bit 31");
        assert_eq!(raw & (0b11 << 4), 0b10 << 4, "AddrInc must be bits 5:4");
        assert_eq!(raw & 0b111, 0b101, "SIZE must be bits 2:0");

        let csw = CSW::try_from(raw).unwrap();
        assert!(csw.DbgSwEnable());
        assert_eq!(csw.AddrInc(), AddressIncrement::Packed);
        assert_eq!(csw.SIZE(), DataSize::U256);
    }

    #[test]
    fn base_baseaddr_is_shifted() {
        let mut base = BASE::try_from(0).unwrap();
        base.set_BASEADDR(0xA_BCDE); // 20-bit value lives in bits 31:12
        base.set_present(true);

        let raw: u32 = base.into();
        assert_eq!(raw & 0xFFFF_F000, 0xA_BCDE << 12);
        assert_eq!(raw & 1, 1);
        assert_eq!(BASE::try_from(raw).unwrap().BASEADDR(), 0xA_BCDE);
    }

    #[test]
    fn idr_designer_splits_cc_and_id() {
        let mut idr = IDR::try_from(0).unwrap();
        idr.set_DESIGNER(jep106::JEP106Code::new(4, 0x3b));
        idr.set_CLASS(ApClass::MemAp);

        let idr = IDR::try_from(u32::from(idr)).unwrap();
        assert_eq!(idr.DESIGNER(), jep106::JEP106Code::new(4, 0x3b));
        assert_eq!(idr.CLASS(), ApClass::MemAp);
    }

    #[test]
    fn unknown_enum_encoding_is_preserved() {
        use crate::architecture::arm::ap::ApType;

        // SIZE = 0b111 is not a defined DataSize; it round-trips as Unknown(7).
        let csw = CSW::from_raw(0b111);
        assert_eq!(csw.SIZE(), DataSize::Unknown(0b111));
        assert_eq!(u32::from(csw) & 0b111, 0b111);

        // TYPE = 0x3 is a gap in ApType.
        assert_eq!(IDR::from_raw(0x3).TYPE(), ApType::Unknown(0x3));

        // A defined encoding still decodes to its variant.
        assert_eq!(CSW::from_raw(0b010).SIZE(), DataSize::U32);
    }
}
