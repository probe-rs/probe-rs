//! Generic access port

use super::{AccessPort, ApRegister, Register};
use crate::architecture::arm::{communication_interface::RegisterParseError, ApAddress};
use enum_primitive_derive::Primitive;
use num_traits::cast::{FromPrimitive, ToPrimitive};

/// Describes the class of an access port defined in the [`ARM Debug Interface v5.2`](https://developer.arm.com/documentation/ihi0031/f/?lang=en) specification.
#[derive(Debug, Primitive, Clone, Copy, PartialEq, Eq)]
pub enum ApClass {
    /// This describes a custom AP that is vendor defined and not defined by ARM
    Undefined = 0b0000,
    /// The standard ARM COM-AP defined in the [`ARM Debug Interface v5.2`](https://developer.arm.com/documentation/ihi0031/f/?lang=en) specification.
    ComAp = 0b0001,
    /// The standard ARM MEM-AP defined  in the [`ARM Debug Interface v5.2`](https://developer.arm.com/documentation/ihi0031/f/?lang=en) specification
    MemAp = 0b1000,
}

impl Default for ApClass {
    fn default() -> Self {
        ApClass::Undefined
    }
}

/// The type of AP defined in the [`ARM Debug Interface v5.2`](https://developer.arm.com/documentation/ihi0031/f/?lang=en) specification.
/// The different types correspond to the different access/memory buses of ARM cores.
#[allow(non_camel_case_types)]
#[derive(Debug, Primitive, Clone, Copy, PartialEq, Eq)]
pub enum ApType {
    /// This is the most basic AP that is included in most MCUs and uses SWD or JTAG as an access bus.
    JtagComAp = 0x0,
    /// A AMBA based AHB3 AP (see E1.5).
    AmbaAhb3 = 0x1,
    /// A AMBA based AHB2 and AHB3 AP (see E1.8).
    AmbaAhb2Ahb3 = 0x2,
    /// A AMBA based AXI3 and AXI4 AP (see E1.2).
    AmbaAxi3Axi4 = 0x4,
    /// A AMBA based AHB5 AP (see E1.6).
    AmbaAhb5 = 0x5,
    /// A AMBA based AHB4 AP (see E1.3).
    AmbaAhb4 = 0x6,
    /// A AMBA based AXI5 AP (see E1.4).
    AmbaAxi5 = 0x7,
    /// A AMBA based protected AHB5 AP (see E1.7).
    AmbaAhb5Hprot = 0x8,
}

impl Default for ApType {
    fn default() -> Self {
        ApType::JtagComAp
    }
}

define_ap!(
    /// A generic access port which implements just the register every access port has to implement
    /// to be compliant with the ADI 5.2 specification.
    GenericAp
);

define_ap_register!(
    type: GenericAp,
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
        DESIGNER: u16,
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
        DESIGNER: ((value >> 17) & 0x7FF) as u16,
        CLASS: ApClass::from_u8(((value >> 13) & 0x0F) as u8).ok_or_else(|| RegisterParseError::new("IDR", value))?,
        _RES0: 0,
        VARIANT: ((value >> 4) & 0x0F) as u8,
        TYPE: ApType::from_u8((value & 0x0F) as u8).ok_or_else(|| RegisterParseError::new("IDR", value))?
    }),
    to: value => (u32::from(value.REVISION) << 28)
        | (u32::from(value.DESIGNER) << 17)
        | (value.CLASS.to_u32().unwrap() << 13)
        | (u32::from(value.VARIANT) << 4)
        | (value.TYPE.to_u32().unwrap())
);
