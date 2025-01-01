//! Register definitions for AArch64.

use std::sync::LazyLock;

use crate::{
    core::{RegisterDataType, RegisterRole, UnwindRule},
    CoreRegister, CoreRegisters, RegisterId,
};

pub(crate) const PC: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("PC"), RegisterRole::ProgramCounter],
    id: RegisterId(32),
    data_type: RegisterDataType::UnsignedInteger(64),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FP: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("X29"), RegisterRole::FramePointer],
    id: RegisterId(29),
    data_type: RegisterDataType::UnsignedInteger(64),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const SP: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("SP"), RegisterRole::StackPointer],
    id: RegisterId(31),
    data_type: RegisterDataType::UnsignedInteger(64),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const RA: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("X30"), RegisterRole::ReturnAddress],
    id: RegisterId(30),
    data_type: RegisterDataType::UnsignedInteger(64),
    unwind_rule: UnwindRule::Clear,
};

/// AArch64 core registers
pub static AARCH64_CORE_REGISTERS: LazyLock<CoreRegisters> =
    LazyLock::new(|| CoreRegisters::new(AARCH64_CORE_REGISTERS_SET.iter().collect()));

pub static AARCH64_CORE_REGSISTERS_SET: &[CoreRegister] = &[
    CoreRegister {
        roles: &[
            RegisterRole::Core("X0"),
            RegisterRole::Argument("a0"),
            RegisterRole::Return("r0"),
        ],
        id: RegisterId(0),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[
            RegisterRole::Core("X1"),
            RegisterRole::Argument("a1"),
            RegisterRole::Return("r1"),
        ],
        id: RegisterId(1),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X2"), RegisterRole::Argument("a2")],
        id: RegisterId(2),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X3"), RegisterRole::Argument("a3")],
        id: RegisterId(3),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X4"), RegisterRole::Argument("a4")],
        id: RegisterId(4),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X5"), RegisterRole::Argument("a5")],
        id: RegisterId(5),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X6"), RegisterRole::Argument("a6")],
        id: RegisterId(6),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X7"), RegisterRole::Argument("a7")],
        id: RegisterId(7),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        // Indirect result location register.
        roles: &[RegisterRole::Core("X8"), RegisterRole::Other("XR")],
        id: RegisterId(8),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X9")],
        id: RegisterId(9),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X10")],
        id: RegisterId(10),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X11")],
        id: RegisterId(11),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X12")],
        id: RegisterId(12),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X13")],
        id: RegisterId(13),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X14")],
        id: RegisterId(14),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X15")],
        id: RegisterId(15),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X16")],
        id: RegisterId(16),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X17")],
        id: RegisterId(17),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X18")],
        id: RegisterId(18),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X19")],
        id: RegisterId(19),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X20")],
        id: RegisterId(20),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X21")],
        id: RegisterId(21),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X22")],
        id: RegisterId(22),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X23")],
        id: RegisterId(23),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X24")],
        id: RegisterId(24),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X25")],
        id: RegisterId(25),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X26")],
        id: RegisterId(26),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X27")],
        id: RegisterId(27),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("X28")],
        id: RegisterId(28),
        data_type: RegisterDataType::UnsignedInteger(64),
        unwind_rule: UnwindRule::Clear,
    },
    FP,
    RA,
    SP,
    PC,
    CoreRegister {
        roles: &[RegisterRole::Core("PSTATE"), RegisterRole::ProcessorStatus],
        id: RegisterId(33),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v0"), RegisterRole::FloatingPoint],
        id: RegisterId(34),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v1"), RegisterRole::FloatingPoint],
        id: RegisterId(35),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v2"), RegisterRole::FloatingPoint],
        id: RegisterId(36),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v3"), RegisterRole::FloatingPoint],
        id: RegisterId(37),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v4"), RegisterRole::FloatingPoint],
        id: RegisterId(38),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v5"), RegisterRole::FloatingPoint],
        id: RegisterId(39),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v6"), RegisterRole::FloatingPoint],
        id: RegisterId(40),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v7"), RegisterRole::FloatingPoint],
        id: RegisterId(41),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v8"), RegisterRole::FloatingPoint],
        id: RegisterId(42),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v9"), RegisterRole::FloatingPoint],
        id: RegisterId(43),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v10"), RegisterRole::FloatingPoint],
        id: RegisterId(44),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v11"), RegisterRole::FloatingPoint],
        id: RegisterId(45),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v12"), RegisterRole::FloatingPoint],
        id: RegisterId(46),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v13"), RegisterRole::FloatingPoint],
        id: RegisterId(47),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v14"), RegisterRole::FloatingPoint],
        id: RegisterId(48),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v15"), RegisterRole::FloatingPoint],
        id: RegisterId(49),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v16"), RegisterRole::FloatingPoint],
        id: RegisterId(50),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v17"), RegisterRole::FloatingPoint],
        id: RegisterId(51),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v18"), RegisterRole::FloatingPoint],
        id: RegisterId(52),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v19"), RegisterRole::FloatingPoint],
        id: RegisterId(53),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v20"), RegisterRole::FloatingPoint],
        id: RegisterId(54),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v21"), RegisterRole::FloatingPoint],
        id: RegisterId(55),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v22"), RegisterRole::FloatingPoint],
        id: RegisterId(56),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v23"), RegisterRole::FloatingPoint],
        id: RegisterId(57),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v24"), RegisterRole::FloatingPoint],
        id: RegisterId(58),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v25"), RegisterRole::FloatingPoint],
        id: RegisterId(59),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v26"), RegisterRole::FloatingPoint],
        id: RegisterId(60),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v27"), RegisterRole::FloatingPoint],
        id: RegisterId(61),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v28"), RegisterRole::FloatingPoint],
        id: RegisterId(62),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v29"), RegisterRole::FloatingPoint],
        id: RegisterId(63),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v30"), RegisterRole::FloatingPoint],
        id: RegisterId(64),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("v31"), RegisterRole::FloatingPoint],
        id: RegisterId(65),
        data_type: RegisterDataType::FloatingPoint(128),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[
            RegisterRole::Core("FPSR"),
            RegisterRole::FloatingPointStatus,
        ],
        id: RegisterId(66),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[
            RegisterRole::Core("FPCR"),
            RegisterRole::Other("Floating Point Control"),
        ],
        id: RegisterId(67),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
];
