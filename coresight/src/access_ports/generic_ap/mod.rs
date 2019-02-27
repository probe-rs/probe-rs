use enum_primitive_derive::Primitive;
use num_traits::cast::{
    FromPrimitive,
    ToPrimitive,
};
use crate::ap_access::AccessPort;
use crate::common::Register;
use crate::access_ports::APRegister;

#[allow(non_camel_case_types)]
#[derive(Debug, Primitive, Clone, Copy)]
pub enum APClass {
    Undefined = 0b0000,
    COMAP = 0b0001,
    MEMAP = 0b1000,
}

impl Default for APClass {
    fn default() -> Self { APClass::Undefined }
}

#[allow(non_camel_case_types)]
#[derive(Debug, Primitive, Clone, Copy)]
pub enum APType {
    JTAG_COM_AP = 0x0,
    AMBA_AHB3 = 0x1,
    AMBA_APB2_APB3 = 0x2,
    AMBA_AXI3_AXI4 = 0x4,
    AMBA_AHB5 = 0x5,
    AMBA_AHB4 = 0x6,
}

impl Default for APType {
    fn default() -> Self { APType::JTAG_COM_AP }
}

define_ap!(GenericAP);

define_ap_register!(GenericAP, IDR, 0x0FC, [
        (REVISION: u8),
        (DESIGNER: u8),
        (CLASS: APClass),
        (_RES0: u8),
        (VARIANT: u8),
        (TYPE: APType),
    ],
    value,
    IDR {
        REVISION: ((value >> 28) & 0x0F) as u8,
        DESIGNER: ((value >> 17) & 0x7FF) as u8,
        CLASS:    APClass::from_u8(((value >> 13) & 0x0F) as u8).unwrap(),
        _RES0:     0,
        VARIANT:  ((value >> 4) & 0x0F) as u8,
        TYPE:     APType::from_u8((value & 0x0F) as u8).unwrap()
    },
      (u32::from(value.REVISION       ) << 28)
    | (u32::from(value.DESIGNER       ) << 17)
    | (value.CLASS.to_u32().unwrap()    << 13)
    | (u32::from(value.VARIANT        ) <<  4)
    | (value.TYPE.to_u32().unwrap()          )
);

define_ap_register!(GenericAP, BASE, 0x0F8, [
        (BASEADDR: u32),
        (_RES0: u8),
        (Format: u8),
        (P: u8),
    ],
    value,
    BASE {
        BASEADDR: value & 0xFFFFF000,
        _RES0:    0,
        Format:   ((value >> 1) & 0x01) as u8,
        P:        (value & 0x01) as u8
    },
      (u32::from(value.BASEADDR       ) << 12)
    // _RES0
    | (u32::from(value.Format       ) << 1)
    | (u32::from(value.P))
);