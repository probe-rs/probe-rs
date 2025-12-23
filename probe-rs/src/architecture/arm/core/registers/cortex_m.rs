//! General Cortex-M registers present on all Cortex-M cores.

use std::sync::LazyLock;

use crate::{
    CoreRegister, CoreRegisters, RegisterId,
    core::{RegisterDataType, RegisterRole, UnwindRule},
};

/// Program counter (PC) register.
pub const PC: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R15"), RegisterRole::ProgramCounter],
    id: RegisterId(15),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::SpecialRule,
};

/// Frame pointer (FP) register.
pub const FP: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R7"), RegisterRole::FramePointer],
    id: RegisterId(7),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Preserve,
};

/// Stack pointer (SP) register.
pub const SP: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R13"), RegisterRole::StackPointer],
    id: RegisterId(13),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Preserve,
};

/// Return address (RA) register.
pub const RA: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R14"), RegisterRole::ReturnAddress],
    id: RegisterId(14),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::SpecialRule,
};

/// xPSR register, the combination of APSR, IPSR, and EPSR.
pub const XPSR: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("XPSR"), RegisterRole::ProcessorStatus],
    id: RegisterId(0b1_0000),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Preserve,
};

/// All of the Cortex-M core registers.
pub static CORTEX_M_CORE_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(CORTEX_M_COMMON_REGS_SET)
            .collect::<Vec<_>>(),
    )
});

/// Cortex-M registers with floating point extension.
pub static CORTEX_M_WITH_FP_CORE_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(CORTEX_M_COMMON_REGS_SET)
            .chain(CORTEX_M_WITH_FP_REGS_SET)
            .collect(),
    )
});

pub(crate) static ARM32_COMMON_REGS_SET: &[CoreRegister] = &[
    CoreRegister {
        roles: &[
            RegisterRole::Core("R0"),
            RegisterRole::Argument("a1"),
            RegisterRole::Return("r1"),
        ],
        id: RegisterId(0),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[
            RegisterRole::Core("R1"),
            RegisterRole::Argument("a2"),
            RegisterRole::Return("r2"),
        ],
        id: RegisterId(1),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("R2"), RegisterRole::Argument("a3")],
        id: RegisterId(2),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("R3"), RegisterRole::Argument("a4")],
        id: RegisterId(3),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("R4")],
        id: RegisterId(4),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("R5")],
        id: RegisterId(5),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("R6")],
        id: RegisterId(6),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    FP,
    CoreRegister {
        roles: &[RegisterRole::Core("R8")],
        id: RegisterId(8),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("R9")],
        id: RegisterId(9),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("R10")],
        id: RegisterId(10),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("R11")],
        id: RegisterId(11),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("R12")],
        id: RegisterId(12),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    SP,
    RA,
    PC,
];

pub(crate) static CORTEX_M_COMMON_REGS_SET: &[CoreRegister] = &[
    CoreRegister {
        roles: &[RegisterRole::Core("MSP"), RegisterRole::MainStackPointer],
        id: RegisterId(0b10001),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("PSP"), RegisterRole::ProcessStackPointer],
        id: RegisterId(0b10010),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    XPSR,
    // CONTROL bits [31:24], FAULTMASK bits [23:16],
    // BASEPRI bits [15:8], and PRIMASK bits [7:0]
    CoreRegister {
        roles: &[RegisterRole::Core("EXTRA"), RegisterRole::Other("EXTRA")],
        id: RegisterId(0b10100),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::SpecialRule,
    },
];

pub(crate) static CORTEX_M_WITH_FP_REGS_SET: &[CoreRegister] = &[
    CoreRegister {
        roles: &[
            RegisterRole::Core("FPSCR"),
            RegisterRole::FloatingPointStatus,
        ],
        id: RegisterId(33),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::SpecialRule,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S0"), RegisterRole::FloatingPoint],
        id: RegisterId(64),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S1"), RegisterRole::FloatingPoint],
        id: RegisterId(65),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S2"), RegisterRole::FloatingPoint],
        id: RegisterId(66),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S3"), RegisterRole::FloatingPoint],
        id: RegisterId(67),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S4"), RegisterRole::FloatingPoint],
        id: RegisterId(68),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S5"), RegisterRole::FloatingPoint],
        id: RegisterId(69),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S6"), RegisterRole::FloatingPoint],
        id: RegisterId(70),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S7"), RegisterRole::FloatingPoint],
        id: RegisterId(71),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S8"), RegisterRole::FloatingPoint],
        id: RegisterId(72),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S9"), RegisterRole::FloatingPoint],
        id: RegisterId(73),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S10"), RegisterRole::FloatingPoint],
        id: RegisterId(74),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S11"), RegisterRole::FloatingPoint],
        id: RegisterId(75),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S12"), RegisterRole::FloatingPoint],
        id: RegisterId(76),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S13"), RegisterRole::FloatingPoint],
        id: RegisterId(77),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S14"), RegisterRole::FloatingPoint],
        id: RegisterId(78),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S15"), RegisterRole::FloatingPoint],
        id: RegisterId(79),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S16"), RegisterRole::FloatingPoint],
        id: RegisterId(80),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S17"), RegisterRole::FloatingPoint],
        id: RegisterId(81),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S18"), RegisterRole::FloatingPoint],
        id: RegisterId(82),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S19"), RegisterRole::FloatingPoint],
        id: RegisterId(83),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S20"), RegisterRole::FloatingPoint],
        id: RegisterId(84),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S21"), RegisterRole::FloatingPoint],
        id: RegisterId(85),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S22"), RegisterRole::FloatingPoint],
        id: RegisterId(86),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S23"), RegisterRole::FloatingPoint],
        id: RegisterId(87),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S24"), RegisterRole::FloatingPoint],
        id: RegisterId(88),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S25"), RegisterRole::FloatingPoint],
        id: RegisterId(89),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S26"), RegisterRole::FloatingPoint],
        id: RegisterId(90),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S27"), RegisterRole::FloatingPoint],
        id: RegisterId(91),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S28"), RegisterRole::FloatingPoint],
        id: RegisterId(92),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S29"), RegisterRole::FloatingPoint],
        id: RegisterId(93),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S30"), RegisterRole::FloatingPoint],
        id: RegisterId(94),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("S31"), RegisterRole::FloatingPoint],
        id: RegisterId(95),
        data_type: RegisterDataType::FloatingPoint(32),
        unwind_rule: UnwindRule::Preserve,
    },
];
