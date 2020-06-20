use crate::core::RegisterDescription;
use crate::{
    core::{RegisterFile, RegisterKind},
    CoreRegisterAddress,
};

macro_rules! data_register {
    ($i:ident, $addr:expr, $name:expr) => {
        struct $i(u32);

        impl DebugRegister for $i {
            const ADDRESS: u8 = $addr;
            const NAME: &'static str = $name;
        }

        impl From<$i> for u32 {
            fn from(register: $i) -> Self {
                register.0
            }
        }

        impl From<u32> for $i {
            fn from(value: u32) -> Self {
                Self(value)
            }
        }
    };

    (pub $i:ident, $addr:expr, $name:expr) => {
        pub struct $i(u32);

        impl DebugRegister for $i {
            const ADDRESS: u8 = $addr;
            const NAME: &'static str = $name;
        }

        impl From<$i> for u32 {
            fn from(register: $i) -> Self {
                register.0
            }
        }

        impl From<u32> for $i {
            fn from(value: u32) -> Self {
                Self(value)
            }
        }
    };
}

static PC: RegisterDescription = RegisterDescription {
    name: "pc",
    kind: RegisterKind::PC,
    /// This is a CSR register
    address: CoreRegisterAddress(0x7b1),
};

static RA: RegisterDescription = RegisterDescription {
    name: "ra",
    kind: RegisterKind::General,
    /// This is a CSR register
    address: CoreRegisterAddress(0x1001),
};

static SP: RegisterDescription = RegisterDescription {
    name: "sp",
    kind: RegisterKind::General,
    /// This is a CSR register
    address: CoreRegisterAddress(0x1002),
};

pub static S0: RegisterDescription = RegisterDescription {
    name: "s0",
    kind: RegisterKind::General,
    /// This is a CSR register
    address: CoreRegisterAddress(0x1008),
};

pub static S1: RegisterDescription = RegisterDescription {
    name: "s1",
    kind: RegisterKind::General,
    /// This is a CSR register
    address: CoreRegisterAddress(0x1009),
};

pub(super) static RISCV_REGISTERS: RegisterFile = RegisterFile {
    platform_registers: &[
        RegisterDescription {
            name: "x0",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1000),
        },
        RegisterDescription {
            name: "x1",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1001),
        },
        RegisterDescription {
            name: "x2",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1002),
        },
        RegisterDescription {
            name: "x3",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1003),
        },
        RegisterDescription {
            name: "x4",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1004),
        },
        RegisterDescription {
            name: "x5",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1005),
        },
        RegisterDescription {
            name: "x6",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1006),
        },
        RegisterDescription {
            name: "x7",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1007),
        },
        RegisterDescription {
            name: "x8",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1008),
        },
        RegisterDescription {
            name: "x9",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1009),
        },
        RegisterDescription {
            name: "x10",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100A),
        },
        RegisterDescription {
            name: "x11",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100B),
        },
        RegisterDescription {
            name: "x12",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100C),
        },
        RegisterDescription {
            name: "x13",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100D),
        },
        RegisterDescription {
            name: "x14",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100E),
        },
    ],

    program_counter: &PC,

    return_address: &RA,

    stack_pointer: &SP,

    argument_registers: &[
        RegisterDescription {
            name: "a0",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100A),
        },
        RegisterDescription {
            name: "a1",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100B),
        },
        RegisterDescription {
            name: "a2",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100C),
        },
        RegisterDescription {
            name: "a3",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100D),
        },
        RegisterDescription {
            name: "a4",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100E),
        },
        RegisterDescription {
            name: "a5",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100F),
        },
        RegisterDescription {
            name: "a6",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1010),
        },
        RegisterDescription {
            name: "a7",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1011),
        },
    ],

    result_registers: &[
        RegisterDescription {
            name: "a0",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100A),
        },
        RegisterDescription {
            name: "a1",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100B),
        },
    ],
};
