//! RISC-V register descriptions.

use std::sync::LazyLock;

use crate::{
    CoreRegisters,
    core::{CoreRegister, RegisterDataType, RegisterId, RegisterRole, UnwindRule},
};

/// The program counter register.
pub const PC: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("pc"), RegisterRole::ProgramCounter],
    id: RegisterId(0x7b1),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("x8"),
        RegisterRole::FramePointer,
        RegisterRole::Other("s0"),
    ],
    id: RegisterId(0x1008),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const SP: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x2"), RegisterRole::StackPointer],
    id: RegisterId(0x1002),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const RA: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x1"), RegisterRole::ReturnAddress],
    id: RegisterId(0x1001),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

// S0 and S1 need to be referenceable as constants in other parts of the architecture specific code.

/// The zero register, x0.
pub const ZERO: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x0"), RegisterRole::Other("zero")],
    id: RegisterId(0x1000),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};
/// The first saved register, s0. Used as the frame pointer
pub const S0: CoreRegister = FP;
/// The second saved register, s1.
pub const S1: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x9"), RegisterRole::Other("s1")],
    id: RegisterId(0x1009),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

/// The RISCV core registers.
pub static RISCV_CORE_REGISTERS: LazyLock<CoreRegisters> =
    LazyLock::new(|| CoreRegisters::new(RISCV_REGISTERS_SET.iter().collect()));

static RISCV_REGISTERS_SET: &[CoreRegister] = &[
    ZERO,
    RA,
    SP,
    CoreRegister {
        roles: &[RegisterRole::Core("x3"), RegisterRole::Other("gp")],
        id: RegisterId(0x1003),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x4"), RegisterRole::Other("tp")],
        id: RegisterId(0x1004),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x5"), RegisterRole::Other("t0")],
        id: RegisterId(0x1005),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x6"), RegisterRole::Other("t1")],
        id: RegisterId(0x1006),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x7"), RegisterRole::Other("t2")],
        id: RegisterId(0x1007),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    FP,
    S1,
    CoreRegister {
        roles: &[
            RegisterRole::Core("x10"),
            RegisterRole::Argument("a0"),
            RegisterRole::Return("r0"),
        ],
        id: RegisterId(0x100A),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[
            RegisterRole::Core("x11"),
            RegisterRole::Argument("a1"),
            RegisterRole::Return("r1"),
        ],
        id: RegisterId(0x100B),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x12"), RegisterRole::Argument("a2")],
        id: RegisterId(0x100C),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x13"), RegisterRole::Argument("a3")],
        id: RegisterId(0x100D),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x14"), RegisterRole::Argument("a4")],
        id: RegisterId(0x100E),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x15"), RegisterRole::Argument("a5")],
        id: RegisterId(0x100F),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x16"), RegisterRole::Argument("a6")],
        id: RegisterId(0x1010),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x17"), RegisterRole::Argument("a7")],
        id: RegisterId(0x1011),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x18"), RegisterRole::Other("s2")],
        id: RegisterId(0x1012),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x19"), RegisterRole::Other("s3")],
        id: RegisterId(0x1013),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x20"), RegisterRole::Other("s4")],
        id: RegisterId(0x1014),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x21"), RegisterRole::Other("s5")],
        id: RegisterId(0x1015),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x22"), RegisterRole::Other("s6")],
        id: RegisterId(0x1016),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x23"), RegisterRole::Other("s7")],
        id: RegisterId(0x1017),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x24"), RegisterRole::Other("s8")],
        id: RegisterId(0x1018),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x25"), RegisterRole::Other("s9")],
        id: RegisterId(0x1019),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x26"), RegisterRole::Other("s10")],
        id: RegisterId(0x101A),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x27"), RegisterRole::Other("s11")],
        id: RegisterId(0x101B),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x28"), RegisterRole::Other("t3")],
        id: RegisterId(0x101C),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x29"), RegisterRole::Other("t4")],
        id: RegisterId(0x101D),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x30"), RegisterRole::Other("t5")],
        id: RegisterId(0x101E),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("x31"), RegisterRole::Other("t6")],
        id: RegisterId(0x101F),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    PC,
    // TODO: Add FPU registers
];
