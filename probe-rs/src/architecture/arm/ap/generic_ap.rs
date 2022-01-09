//! Generic access port

use super::{AccessPort, ApRegister, Register};
use crate::architecture::arm::ApAddress;
use enum_primitive_derive::Primitive;
use num_traits::cast::{FromPrimitive, ToPrimitive};

#[derive(Debug, Primitive, Clone, Copy, PartialEq)]
pub enum ApClass {
    Undefined = 0b0000,
    ComAp = 0b0001,
    MemAp = 0b1000,
}

impl Default for ApClass {
    fn default() -> Self {
        ApClass::Undefined
    }
}

#[allow(non_camel_case_types)]
#[derive(Debug, Primitive, Clone, Copy, PartialEq)]
pub enum ApType {
    JtagComAp = 0x0,
    AmbaAhb3 = 0x1,
    AmbaAhb2Ahb3 = 0x2,
    AmbaAxi3Axi4 = 0x4,
    AmbaAhb5 = 0x5,
    AmbaAhb4 = 0x6,
    AmbaAxi5 = 0x7,
    AmbaAhb5Hprot = 0x8,
}

impl Default for ApType {
    fn default() -> Self {
        ApType::JtagComAp
    }
}

define_ap!(GenericAp);

define_ap_register!(
    /// Identification Register
    ///
    /// The identification register is used to identify
    /// an AP.
    ///
    /// It has to be present on every AP.
    GenericAp,
    IDR,
    0x0FC,
    [
        (REVISION: u8),
        (DESIGNER: u16),
        (CLASS: ApClass),
        (_RES0: u8),
        (VARIANT: u8),
        (TYPE: ApType),
    ],
    value,
    IDR {
        REVISION: ((value >> 28) & 0x0F) as u8,
        DESIGNER: ((value >> 17) & 0x7FF) as u16,
        CLASS: ApClass::from_u8(((value >> 13) & 0x0F) as u8).unwrap_or_default(),
        _RES0: 0,
        VARIANT: ((value >> 4) & 0x0F) as u8,
        TYPE: ApType::from_u8((value & 0x0F) as u8).unwrap()
    },
    (u32::from(value.REVISION) << 28)
        | (u32::from(value.DESIGNER) << 17)
        | (value.CLASS.to_u32().unwrap() << 13)
        | (u32::from(value.VARIANT) << 4)
        | (value.TYPE.to_u32().unwrap())
);
