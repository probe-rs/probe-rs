//! Generic access port

use crate::architecture::arm::communication_interface::RegisterParseError;

/// Describes the class of an access port defined in the [`ARM Debug Interface v5.2`](https://developer.arm.com/documentation/ihi0031/f/?lang=en) specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApClass {
    /// This describes a custom AP that is vendor defined and not defined by ARM
    #[default]
    Undefined = 0b0000,
    /// The standard ARM COM-AP defined in the [`ARM Debug Interface v5.2`](https://developer.arm.com/documentation/ihi0031/f/?lang=en) specification.
    ComAp = 0b0001,
    /// The standard ARM MEM-AP defined  in the [`ARM Debug Interface v5.2`](https://developer.arm.com/documentation/ihi0031/f/?lang=en) specification
    MemAp = 0b1000,
}

impl ApClass {
    /// Tries to create an `ApClass` from a given `u8`.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0b0000 => Some(ApClass::Undefined),
            0b0001 => Some(ApClass::ComAp),
            0b1000 => Some(ApClass::MemAp),
            _ => None,
        }
    }
}

/// The type of AP defined in the [`ARM Debug Interface v5.2`](https://developer.arm.com/documentation/ihi0031/f/?lang=en) specification.
/// You can find the details in the table C1-2 on page C1-146.
/// The different types correspond to the different access/memory buses of ARM cores.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApType {
    /// This is the most basic AP that is included in most MCUs and uses SWD or JTAG as an access bus.
    #[default]
    JtagComAp = 0x0,
    /// A AMBA based AHB3 AP (see E1.5).
    AmbaAhb3 = 0x1,
    /// A AMBA based APB2 and APB3 AP (see E1.8).
    AmbaApb2Apb3 = 0x2,
    /// A AMBA based AXI3 and AXI4 AP (see E1.2).
    AmbaAxi3Axi4 = 0x4,
    /// A AMBA based AHB5 AP (see E1.6).
    AmbaAhb5 = 0x5,
    /// A AMBA based APB4 and APB5 AP (see E1.9).
    AmbaApb4Apb5 = 0x6,
    /// A AMBA based AXI5 AP (see E1.4).
    AmbaAxi5 = 0x7,
    /// A AMBA based AHB5 AP with enhanced HPROT (see E1.7).
    AmbaAhb5Hprot = 0x8,
}

impl ApType {
    /// Tries to create an `ApType` from a given `u8`.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x0 => Some(ApType::JtagComAp),
            0x1 => Some(ApType::AmbaAhb3),
            0x2 => Some(ApType::AmbaApb2Apb3),
            0x4 => Some(ApType::AmbaAxi3Axi4),
            0x5 => Some(ApType::AmbaAhb5),
            0x6 => Some(ApType::AmbaApb4Apb5),
            0x7 => Some(ApType::AmbaAxi5),
            0x8 => Some(ApType::AmbaAhb5Hprot),
            _ => None,
        }
    }
}

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
