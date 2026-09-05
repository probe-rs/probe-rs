//! Xtensa register descriptions.

use std::sync::LazyLock;

use crate::{
    CoreRegister, CoreRegisters, RegisterRole,
    architecture::xtensa::arch::{SpecialRegister, UserRegister},
    core::{RegisterDataType, UnwindRule},
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

/// Processor status register.
pub const PS: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("ps"), RegisterRole::ProcessorStatus],
    id: crate::RegisterId(0xFF01),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

/// Xtensa core registers
pub static XTENSA_CORE_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        XTENSA_REGISTERS_SET
            .iter()
            // Occasionally very useful to observe, but way too much to always include:
            //.chain(XTENSA_EXCEPTION_OPTION_REGISTERS.iter())
            //.chain(XTENSA_HP_INTERRUPT_OPTION_REGISTERS.iter())
            .collect(),
    )
});

/// Xtensa core registers, for cores with the Floating-Point Coprocessor Option
pub static XTENSA_WITH_FP_CORE_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        XTENSA_REGISTERS_SET
            .iter()
            .chain(XTENSA_FP_OPTION_REGISTERS.iter())
            .collect(),
    )
});

static XTENSA_REGISTERS_SET: &[CoreRegister] = &[
    RA,
    SP,
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
    FP,
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
    PC,
    PS,
];

static XTENSA_FP_OPTION_REGISTERS: &[CoreRegister] = &[
    CoreRegister {
        roles: &[RegisterRole::Core("f0"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x0200),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f1"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x0201),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f2"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x0202),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f3"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x0203),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f4"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x0204),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f5"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x0205),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f6"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x0206),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f7"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x0207),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f8"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x0208),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f9"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x0209),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f10"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x020A),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f11"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x020B),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f12"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x020C),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f13"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x020D),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f14"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x020E),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("f15"), RegisterRole::FloatingPoint],
        id: crate::RegisterId(0x020F),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("fcr"), RegisterRole::FloatingPointStatus],
        id: crate::RegisterId(0x0300 | UserRegister::Fcr as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("fsr"), RegisterRole::FloatingPointStatus],
        id: crate::RegisterId(0x0300 | UserRegister::Fsr as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
];

#[allow(unused)]
static XTENSA_EXCEPTION_OPTION_REGISTERS: &[CoreRegister] = &[
    CoreRegister {
        roles: &[RegisterRole::Other("EPC1")],
        id: crate::RegisterId(0x0100 | SpecialRegister::Epc1 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EXCCAUSE")],
        id: crate::RegisterId(0x0100 | SpecialRegister::ExcCause as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EXCSAVE1")],
        id: crate::RegisterId(0x0100 | SpecialRegister::ExcSave1 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EXCVADDR")],
        id: crate::RegisterId(0x0100 | SpecialRegister::ExcVaddr as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("DEPC")],
        id: crate::RegisterId(0x0100 | 192),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
];
#[allow(unused)]
static XTENSA_HP_INTERRUPT_OPTION_REGISTERS: &[CoreRegister] = &[
    CoreRegister {
        roles: &[RegisterRole::Other("EPC2")],
        id: crate::RegisterId(0x0100 | SpecialRegister::Epc2 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EPC3")],
        id: crate::RegisterId(0x0100 | SpecialRegister::Epc3 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EPC4")],
        id: crate::RegisterId(0x0100 | SpecialRegister::Epc4 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EPC5")],
        id: crate::RegisterId(0x0100 | SpecialRegister::Epc5 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EPC6")],
        id: crate::RegisterId(0x0100 | SpecialRegister::Epc6 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EPC7")],
        id: crate::RegisterId(0x0100 | SpecialRegister::Epc7 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EPS2")],
        id: crate::RegisterId(0x0100 | SpecialRegister::Eps2 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EPS3")],
        id: crate::RegisterId(0x0100 | SpecialRegister::Eps3 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EPS4")],
        id: crate::RegisterId(0x0100 | SpecialRegister::Eps4 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EPS5")],
        id: crate::RegisterId(0x0100 | SpecialRegister::Eps5 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EPS6")],
        id: crate::RegisterId(0x0100 | SpecialRegister::Eps6 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EPS7")],
        id: crate::RegisterId(0x0100 | SpecialRegister::Eps7 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EXCSAVE2")],
        id: crate::RegisterId(0x0100 | SpecialRegister::ExcSave2 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EXCSAVE3")],
        id: crate::RegisterId(0x0100 | SpecialRegister::ExcSave3 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EXCSAVE4")],
        id: crate::RegisterId(0x0100 | SpecialRegister::ExcSave4 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EXCSAVE5")],
        id: crate::RegisterId(0x0100 | SpecialRegister::ExcSave5 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EXCSAVE6")],
        id: crate::RegisterId(0x0100 | SpecialRegister::ExcSave6 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Other("EXCSAVE7")],
        id: crate::RegisterId(0x0100 | SpecialRegister::ExcSave7 as u16),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
];
