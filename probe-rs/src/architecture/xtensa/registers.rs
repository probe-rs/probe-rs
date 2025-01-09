//! Xtensa register descriptions.

use std::sync::LazyLock;

use crate::{
    core::{RegisterDataType, UnwindRule},
    CoreRegister, CoreRegisters, RegisterRole,
};

/// The program counter register.
pub const PC: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("pc"), RegisterRole::ProgramCounter],
    id: crate::RegisterId(0xFF00),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

/// The return address register.
pub const RA: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("a0"), RegisterRole::ReturnAddress],
    id: crate::RegisterId(0x0000),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

/// The stack pointer register.
pub const SP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("sp"),
        RegisterRole::Core("a1"),
        RegisterRole::StackPointer,
    ],
    id: crate::RegisterId(0x0001),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

/// The frame pointer register.
pub const FP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("fp"),
        RegisterRole::Core("a7"),
        RegisterRole::FramePointer,
    ],
    id: crate::RegisterId(0x0007),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

/// XTENSA core registers
pub static XTENSA_CORE_REGISTERS: LazyLock<CoreRegisters> =
    LazyLock::new(|| CoreRegisters::new(XTENSA_REGISTERS_SET.iter().collect()));

static XTENSA_REGISTERS_SET: &[CoreRegister] = &[
    RA,
    PC,
    SP,
    FP,
    CoreRegister {
        roles: &[
            RegisterRole::Core("a2"),
            RegisterRole::Argument("a2"),
            RegisterRole::Return("a2"),
        ],
        id: crate::RegisterId(0x0002),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[
            RegisterRole::Core("a3"),
            RegisterRole::Argument("a3"),
            RegisterRole::Return("a3"),
        ],
        id: crate::RegisterId(0x0003),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[
            RegisterRole::Core("a4"),
            RegisterRole::Argument("a4"),
            RegisterRole::Return("a4"),
        ],
        id: crate::RegisterId(0x0004),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[
            RegisterRole::Core("a5"),
            RegisterRole::Argument("a5"),
            RegisterRole::Return("a5"),
        ],
        id: crate::RegisterId(0x0005),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("a6"), RegisterRole::Argument("a6")],
        id: crate::RegisterId(0x0006),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("a8")],
        id: crate::RegisterId(0x0008),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("a9")],
        id: crate::RegisterId(0x0009),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("a10")],
        id: crate::RegisterId(0x000A),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("a11")],
        id: crate::RegisterId(0x000B),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("a12")],
        id: crate::RegisterId(0x000C),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("a13")],
        id: crate::RegisterId(0x000D),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("a14")],
        id: crate::RegisterId(0x000E),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("a15")],
        id: crate::RegisterId(0x000F),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
];
