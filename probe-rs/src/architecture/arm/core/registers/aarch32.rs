//! Register definitions for AARCH32.

use std::sync::LazyLock;

use super::cortex_m::ARM32_COMMON_REGS_SET;
use crate::{
    CoreRegister, CoreRegisters, RegisterId,
    core::{RegisterDataType, RegisterRole, UnwindRule},
};

/// Core registers used in the AARCH32 instruction set.
pub static AARCH32_CORE_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(AARCH32_COMMON_REGS_SET)
            .collect::<Vec<_>>(),
    )
});

/// AArch32 registers with FP16 floating point extension
pub static AARCH32_WITH_FP_16_CORE_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(AARCH32_COMMON_REGS_SET)
            .chain(AARCH32_FP_16_REGS_SET)
            .collect(),
    )
});

/// AArch32 registers with FP16 and FP32 floating point extension
pub static AARCH32_WITH_FP_32_CORE_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(AARCH32_COMMON_REGS_SET)
            .chain(AARCH32_FP_16_REGS_SET)
            .chain(AARCH32_FP_32_REGS_SET)
            .collect(),
    )
});

static AARCH32_COMMON_REGS_SET: &[CoreRegister] = &[CoreRegister {
    roles: &[RegisterRole::Core("CPSR"), RegisterRole::ProcessorStatus],
    id: RegisterId(0b1_0000),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
}];

static AARCH32_FP_16_REGS_SET: &[CoreRegister] = &[
    CoreRegister {
        roles: &[
            RegisterRole::Core("FPSCR"),
            RegisterRole::FloatingPointStatus,
        ],
        id: RegisterId(49),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D0"), RegisterRole::FloatingPoint],
        id: RegisterId(17),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D1"), RegisterRole::FloatingPoint],
        id: RegisterId(18),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D2"), RegisterRole::FloatingPoint],
        id: RegisterId(19),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D3"), RegisterRole::FloatingPoint],
        id: RegisterId(20),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D4"), RegisterRole::FloatingPoint],
        id: RegisterId(21),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D5"), RegisterRole::FloatingPoint],
        id: RegisterId(22),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D6"), RegisterRole::FloatingPoint],
        id: RegisterId(23),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D7"), RegisterRole::FloatingPoint],
        id: RegisterId(24),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D8"), RegisterRole::FloatingPoint],
        id: RegisterId(25),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D9"), RegisterRole::FloatingPoint],
        id: RegisterId(26),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D10"), RegisterRole::FloatingPoint],
        id: RegisterId(27),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D11"), RegisterRole::FloatingPoint],
        id: RegisterId(28),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D12"), RegisterRole::FloatingPoint],
        id: RegisterId(29),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D13"), RegisterRole::FloatingPoint],
        id: RegisterId(30),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D14"), RegisterRole::FloatingPoint],
        id: RegisterId(31),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D15"), RegisterRole::FloatingPoint],
        id: RegisterId(32),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
];

static AARCH32_FP_32_REGS_SET: &[CoreRegister] = &[
    CoreRegister {
        roles: &[RegisterRole::Core("D16"), RegisterRole::FloatingPoint],
        id: RegisterId(33),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D17"), RegisterRole::FloatingPoint],
        id: RegisterId(34),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D18"), RegisterRole::FloatingPoint],
        id: RegisterId(35),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D19"), RegisterRole::FloatingPoint],
        id: RegisterId(36),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D20"), RegisterRole::FloatingPoint],
        id: RegisterId(37),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D21"), RegisterRole::FloatingPoint],
        id: RegisterId(38),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D22"), RegisterRole::FloatingPoint],
        id: RegisterId(39),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D23"), RegisterRole::FloatingPoint],
        id: RegisterId(40),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D24"), RegisterRole::FloatingPoint],
        id: RegisterId(41),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D25"), RegisterRole::FloatingPoint],
        id: RegisterId(42),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D26"), RegisterRole::FloatingPoint],
        id: RegisterId(43),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D27"), RegisterRole::FloatingPoint],
        id: RegisterId(44),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D28"), RegisterRole::FloatingPoint],
        id: RegisterId(45),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D29"), RegisterRole::FloatingPoint],
        id: RegisterId(46),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D30"), RegisterRole::FloatingPoint],
        id: RegisterId(47),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("D31"), RegisterRole::FloatingPoint],
        id: RegisterId(48),
        data_type: RegisterDataType::FloatingPoint(64),
        unwind_rule: UnwindRule::Clear,
    },
];
