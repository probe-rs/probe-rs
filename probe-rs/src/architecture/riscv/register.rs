use crate::core::RegisterDescription;
use crate::{
    core::{RegisterFile, RegisterKind},
    CoreRegisterAddress,
};

macro_rules! data_register {
    ($(#[$outer:meta])* $i:ident, $addr:expr, $name:expr) => {
        $(#[$outer])*
        #[derive(Debug, Copy, Clone)]
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
        #[derive(Debug, Copy, Clone)]
        #[doc = concat!(stringify!($name), " register.")]
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
    _kind: RegisterKind::PC,
    /// This is a CSR register
    address: CoreRegisterAddress(0x7b1),
};

static RA: RegisterDescription = RegisterDescription {
    name: "ra",
    _kind: RegisterKind::General,
    /// This is a CSR register
    address: CoreRegisterAddress(0x1001),
};

static SP: RegisterDescription = RegisterDescription {
    name: "sp",
    _kind: RegisterKind::General,
    /// This is a CSR register
    address: CoreRegisterAddress(0x1002),
};

static FP: RegisterDescription = RegisterDescription {
    name: "fp",
    _kind: RegisterKind::General,
    /// This is a CSR register
    address: CoreRegisterAddress(0x1008),
};

pub static S0: RegisterDescription = RegisterDescription {
    name: "s0",
    _kind: RegisterKind::General,
    /// This is a CSR register
    address: CoreRegisterAddress(0x1008),
};

pub static S1: RegisterDescription = RegisterDescription {
    name: "s1",
    _kind: RegisterKind::General,
    /// This is a CSR register
    address: CoreRegisterAddress(0x1009),
};

pub(super) static RISCV_REGISTERS: RegisterFile = RegisterFile {
    platform_registers: &[
        RegisterDescription {
            name: "x0",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1000),
        },
        RegisterDescription {
            name: "x1",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1001),
        },
        RegisterDescription {
            name: "x2",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1002),
        },
        RegisterDescription {
            name: "x3",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1003),
        },
        RegisterDescription {
            name: "x4",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1004),
        },
        RegisterDescription {
            name: "x5",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1005),
        },
        RegisterDescription {
            name: "x6",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1006),
        },
        RegisterDescription {
            name: "x7",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1007),
        },
        RegisterDescription {
            name: "x8",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1008),
        },
        RegisterDescription {
            name: "x9",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1009),
        },
        RegisterDescription {
            name: "x10",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100A),
        },
        RegisterDescription {
            name: "x11",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100B),
        },
        RegisterDescription {
            name: "x12",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100C),
        },
        RegisterDescription {
            name: "x13",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100D),
        },
        RegisterDescription {
            name: "x14",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100E),
        },
        RegisterDescription {
            name: "x15",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100F),
        },
        RegisterDescription {
            name: "x16",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1010),
        },
        RegisterDescription {
            name: "x17",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1011),
        },
        RegisterDescription {
            name: "x18",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1012),
        },
        RegisterDescription {
            name: "x19",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1013),
        },
        RegisterDescription {
            name: "x20",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1014),
        },
        RegisterDescription {
            name: "x21",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1015),
        },
        RegisterDescription {
            name: "x22",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1016),
        },
        RegisterDescription {
            name: "x23",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1017),
        },
        RegisterDescription {
            name: "x24",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1018),
        },
        RegisterDescription {
            name: "x25",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1019),
        },
        RegisterDescription {
            name: "x26",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x101A),
        },
        RegisterDescription {
            name: "x27",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x101B),
        },
        RegisterDescription {
            name: "x28",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x101C),
        },
        RegisterDescription {
            name: "x29",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x101D),
        },
        RegisterDescription {
            name: "x30",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x101E),
        },
        RegisterDescription {
            name: "x31",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x101F),
        },
    ],

    program_counter: &PC,

    return_address: &RA,

    stack_pointer: &SP,

    frame_pointer: &FP,

    argument_registers: &[
        RegisterDescription {
            name: "a0",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100A),
        },
        RegisterDescription {
            name: "a1",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100B),
        },
        RegisterDescription {
            name: "a2",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100C),
        },
        RegisterDescription {
            name: "a3",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100D),
        },
        RegisterDescription {
            name: "a4",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100E),
        },
        RegisterDescription {
            name: "a5",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100F),
        },
        RegisterDescription {
            name: "a6",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1010),
        },
        RegisterDescription {
            name: "a7",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x1011),
        },
    ],

    result_registers: &[
        RegisterDescription {
            name: "a0",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100A),
        },
        RegisterDescription {
            name: "a1",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0x100B),
        },
    ],

    psp: None,
    msp: None,
    extra: None,
};
