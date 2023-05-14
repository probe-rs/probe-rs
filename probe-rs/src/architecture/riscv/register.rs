use crate::core::{RegisterDataType, RegisterDescription, RegisterFile, RegisterId, RegisterKind};

const PC: RegisterDescription = RegisterDescription {
    name: "pc",
    _kind: RegisterKind::PC,
    /// This is a CSR register
    id: RegisterId(0x7b1),
    _type: RegisterDataType::UnsignedInteger,
    size_in_bits: 32,
};

const RA: RegisterDescription = RegisterDescription {
    name: "ra",
    _kind: RegisterKind::General,
    /// This is a CSR register
    id: RegisterId(0x1001),
    _type: RegisterDataType::UnsignedInteger,
    size_in_bits: 32,
};

const SP: RegisterDescription = RegisterDescription {
    name: "sp",
    _kind: RegisterKind::General,
    /// This is a CSR register
    id: RegisterId(0x1002),
    _type: RegisterDataType::UnsignedInteger,
    size_in_bits: 32,
};

const FP: RegisterDescription = RegisterDescription {
    name: "fp",
    _kind: RegisterKind::General,
    /// This is a CSR register
    id: RegisterId(0x1008),
    _type: RegisterDataType::UnsignedInteger,
    size_in_bits: 32,
};

pub const S0: RegisterDescription = RegisterDescription {
    name: "s0",
    _kind: RegisterKind::General,
    /// This is a CSR register
    id: RegisterId(0x1008),
    _type: RegisterDataType::UnsignedInteger,
    size_in_bits: 32,
};

pub const S1: RegisterDescription = RegisterDescription {
    name: "s1",
    _kind: RegisterKind::General,
    /// This is a CSR register
    id: RegisterId(0x1009),
    _type: RegisterDataType::UnsignedInteger,
    size_in_bits: 32,
};

pub(super) const RISCV_REGISTERS: RegisterFile = RegisterFile {
    platform_registers: &[
        RegisterDescription {
            name: "x0",
            _kind: RegisterKind::General,
            id: RegisterId(0x1000),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x1",
            _kind: RegisterKind::General,
            id: RegisterId(0x1001),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x2",
            _kind: RegisterKind::General,
            id: RegisterId(0x1002),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x3",
            _kind: RegisterKind::General,
            id: RegisterId(0x1003),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x4",
            _kind: RegisterKind::General,
            id: RegisterId(0x1004),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x5",
            _kind: RegisterKind::General,
            id: RegisterId(0x1005),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x6",
            _kind: RegisterKind::General,
            id: RegisterId(0x1006),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x7",
            _kind: RegisterKind::General,
            id: RegisterId(0x1007),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x8",
            _kind: RegisterKind::General,
            id: RegisterId(0x1008),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x9",
            _kind: RegisterKind::General,
            id: RegisterId(0x1009),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x10",
            _kind: RegisterKind::General,
            id: RegisterId(0x100A),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x11",
            _kind: RegisterKind::General,
            id: RegisterId(0x100B),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x12",
            _kind: RegisterKind::General,
            id: RegisterId(0x100C),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x13",
            _kind: RegisterKind::General,
            id: RegisterId(0x100D),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x14",
            _kind: RegisterKind::General,
            id: RegisterId(0x100E),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x15",
            _kind: RegisterKind::General,
            id: RegisterId(0x100F),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x16",
            _kind: RegisterKind::General,
            id: RegisterId(0x1010),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x17",
            _kind: RegisterKind::General,
            id: RegisterId(0x1011),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x18",
            _kind: RegisterKind::General,
            id: RegisterId(0x1012),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x19",
            _kind: RegisterKind::General,
            id: RegisterId(0x1013),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x20",
            _kind: RegisterKind::General,
            id: RegisterId(0x1014),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x21",
            _kind: RegisterKind::General,
            id: RegisterId(0x1015),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x22",
            _kind: RegisterKind::General,
            id: RegisterId(0x1016),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x23",
            _kind: RegisterKind::General,
            id: RegisterId(0x1017),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x24",
            _kind: RegisterKind::General,
            id: RegisterId(0x1018),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x25",
            _kind: RegisterKind::General,
            id: RegisterId(0x1019),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x26",
            _kind: RegisterKind::General,
            id: RegisterId(0x101A),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x27",
            _kind: RegisterKind::General,
            id: RegisterId(0x101B),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x28",
            _kind: RegisterKind::General,
            id: RegisterId(0x101C),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x29",
            _kind: RegisterKind::General,
            id: RegisterId(0x101D),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x30",
            _kind: RegisterKind::General,
            id: RegisterId(0x101E),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "x31",
            _kind: RegisterKind::General,
            id: RegisterId(0x101F),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
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
            id: RegisterId(0x100A),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "a1",
            _kind: RegisterKind::General,
            id: RegisterId(0x100B),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "a2",
            _kind: RegisterKind::General,
            id: RegisterId(0x100C),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "a3",
            _kind: RegisterKind::General,
            id: RegisterId(0x100D),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "a4",
            _kind: RegisterKind::General,
            id: RegisterId(0x100E),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "a5",
            _kind: RegisterKind::General,
            id: RegisterId(0x100F),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "a6",
            _kind: RegisterKind::General,
            id: RegisterId(0x1010),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "a7",
            _kind: RegisterKind::General,
            id: RegisterId(0x1011),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
    ],

    result_registers: &[
        RegisterDescription {
            name: "a0",
            _kind: RegisterKind::General,
            id: RegisterId(0x100A),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "a1",
            _kind: RegisterKind::General,
            id: RegisterId(0x100B),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
    ],

    psp: None,
    msp: None,
    other: &[],
    psr: None,
    // TODO: Add FPU registers
    fp_registers: None,
    fp_status: None,
};
