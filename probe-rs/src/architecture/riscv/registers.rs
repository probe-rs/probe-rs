use crate::{
    core::{CoreRegister, RegisterDataType, RegisterId, RegisterRole},
    CoreRegisters,
};
use once_cell::sync::Lazy;

/// The program counter register.
pub const PC: CoreRegister = CoreRegister {
    name: "pc",
    roles: &[RegisterRole::ProgramCounter],
    /// This is a CSR register
    id: RegisterId(0x7b1),
    data_type: RegisterDataType::UnsignedInteger(32),
};

pub(crate) const FP: CoreRegister = CoreRegister {
    name: "x8",
    roles: &[RegisterRole::FramePointer, RegisterRole::Other("s0")],
    id: RegisterId(0x1008),
    data_type: RegisterDataType::UnsignedInteger(32),
};

pub(crate) const SP: CoreRegister = CoreRegister {
    name: "x2",
    roles: &[RegisterRole::StackPointer],
    id: RegisterId(0x1002),
    data_type: RegisterDataType::UnsignedInteger(32),
};

pub(crate) const RA: CoreRegister = CoreRegister {
    name: "x1",
    roles: &[RegisterRole::ReturnAddress],
    id: RegisterId(0x1001),
    data_type: RegisterDataType::UnsignedInteger(32),
};

// S0 and S1 need to be referenceable as constants in other parts of the architecture specific code.
pub const S0: CoreRegister = FP;
pub const S1: CoreRegister = CoreRegister {
    name: "x9",
    roles: &[RegisterRole::Other("s1")],
    id: RegisterId(0x1009),
    data_type: RegisterDataType::UnsignedInteger(32),
};

pub(crate) static RISCV_CORE_REGSISTERS: Lazy<CoreRegisters> =
    Lazy::new(|| CoreRegisters::new(RISCV_REGISTERS_SET.iter().collect()));

static RISCV_REGISTERS_SET: &[CoreRegister] = &[
    CoreRegister {
        name: "x0",
        roles: &[RegisterRole::Other("zero")],
        id: RegisterId(0x1000),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    RA,
    SP,
    CoreRegister {
        name: "x3",
        roles: &[RegisterRole::Other("gp")],
        id: RegisterId(0x1003),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x4",
        roles: &[RegisterRole::Other("tp")],
        id: RegisterId(0x1004),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x5",
        roles: &[RegisterRole::Other("t0")],
        id: RegisterId(0x1005),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x6",
        roles: &[RegisterRole::Other("t1")],
        id: RegisterId(0x1006),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x7",
        roles: &[RegisterRole::Other("t2")],
        id: RegisterId(0x1007),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    FP,
    S1,
    CoreRegister {
        name: "x10",
        roles: &[RegisterRole::Argument("a0"), RegisterRole::Return("r0")],
        id: RegisterId(0x100A),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x11",
        roles: &[RegisterRole::Argument("a1"), RegisterRole::Return("r1")],
        id: RegisterId(0x100B),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x12",
        roles: &[RegisterRole::Argument("a2")],
        id: RegisterId(0x100C),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x13",
        roles: &[RegisterRole::Argument("a3")],
        id: RegisterId(0x100D),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x14",
        roles: &[RegisterRole::Argument("a4")],
        id: RegisterId(0x100E),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x15",
        roles: &[RegisterRole::Argument("a5")],
        id: RegisterId(0x100F),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x16",
        roles: &[RegisterRole::Argument("a6")],
        id: RegisterId(0x1010),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x17",
        roles: &[RegisterRole::Argument("a7")],
        id: RegisterId(0x1011),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x18",
        roles: &[RegisterRole::Other("s2")],
        id: RegisterId(0x1012),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x19",
        roles: &[RegisterRole::Other("s3")],
        id: RegisterId(0x1013),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x20",
        roles: &[RegisterRole::Other("s4")],
        id: RegisterId(0x1014),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x21",
        roles: &[RegisterRole::Other("s5")],
        id: RegisterId(0x1015),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x22",
        roles: &[RegisterRole::Other("s6")],
        id: RegisterId(0x1016),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x23",
        roles: &[RegisterRole::Other("s7")],
        id: RegisterId(0x1017),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x24",
        roles: &[RegisterRole::Other("s8")],
        id: RegisterId(0x1018),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x25",
        roles: &[RegisterRole::Other("s9")],
        id: RegisterId(0x1019),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x26",
        roles: &[RegisterRole::Other("s10")],
        id: RegisterId(0x101A),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x27",
        roles: &[RegisterRole::Other("s11")],
        id: RegisterId(0x101B),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x28",
        roles: &[RegisterRole::Other("t3")],
        id: RegisterId(0x101C),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x29",
        roles: &[RegisterRole::Other("t4")],
        id: RegisterId(0x101D),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x30",
        roles: &[RegisterRole::Other("t5")],
        id: RegisterId(0x101E),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "x31",
        roles: &[RegisterRole::Other("t6")],
        id: RegisterId(0x101F),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    PC,
    // TODO: Add FPU registers
];
