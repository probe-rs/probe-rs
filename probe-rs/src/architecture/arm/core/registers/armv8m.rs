//! General Cortex-M registers present on all Cortex-M cores.

use std::sync::LazyLock;

use super::cortex_m::{ARM32_COMMON_REGS_SET, CORTEX_M_COMMON_REGS_SET, CORTEX_M_WITH_FP_REGS_SET};
use crate::{
    CoreRegister, CoreRegisters, RegisterId,
    core::{RegisterDataType, RegisterRole, UnwindRule},
};
/// v8M base + security
pub static V8M_BASE_SEC_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(CORTEX_M_COMMON_REGS_SET.iter())
            .chain(V8M_SECURITY_REGS_SET.iter())
            .collect(),
    )
});

/// v8M base + security + FP
pub static V8M_BASE_SEC_FP_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(CORTEX_M_COMMON_REGS_SET.iter())
            .chain(V8M_SECURITY_REGS_SET.iter())
            .chain(CORTEX_M_WITH_FP_REGS_SET.iter())
            .collect(),
    )
});

/// v8M main
pub static V8M_MAIN_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(CORTEX_M_COMMON_REGS_SET.iter())
            .chain(V8M_MAIN_REGS_SET.iter())
            .collect(),
    )
});

/// v8M main + FP
pub static V8M_MAIN_FP_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(CORTEX_M_COMMON_REGS_SET.iter())
            .chain(V8M_MAIN_REGS_SET.iter())
            .chain(CORTEX_M_WITH_FP_REGS_SET.iter())
            .collect(),
    )
});

/// v8M main + security
pub static V8M_MAIN_SEC_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(CORTEX_M_COMMON_REGS_SET.iter())
            .chain(V8M_MAIN_REGS_SET.iter())
            .chain(V8M_SECURITY_REGS_SET.iter())
            .collect(),
    )
});

/// v8M main + security + FP
pub static V8M_MAIN_SEC_FP_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(CORTEX_M_COMMON_REGS_SET.iter())
            .chain(V8M_MAIN_REGS_SET.iter())
            .chain(V8M_SECURITY_REGS_SET.iter())
            .chain(CORTEX_M_WITH_FP_REGS_SET.iter())
            .collect(),
    )
});

static V8M_MAIN_REGS_SET: &[CoreRegister] = &[
    CoreRegister {
        roles: &[
            RegisterRole::Core("MSPLIM_NS"),
            RegisterRole::Other("MSPLIM_NS"),
        ],
        id: RegisterId(0b00011110),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::SpecialRule,
    },
    CoreRegister {
        roles: &[
            RegisterRole::Core("PSPLIM_NS"),
            RegisterRole::Other("PSPLIM_NS"),
        ],
        id: RegisterId(0b00011111),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::SpecialRule,
    },
];

static V8M_SECURITY_REGS_SET: &[CoreRegister] = &[
    CoreRegister {
        roles: &[RegisterRole::Core("MSP_NS"), RegisterRole::Other("MSP_NS")],
        id: RegisterId(0b00011000),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("PSP_NS"), RegisterRole::Other("PSP_NS")],
        id: RegisterId(0b00011001),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("MSP_S"), RegisterRole::Other("MSP_S")],
        id: RegisterId(0b00011010),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[RegisterRole::Core("PSP_S"), RegisterRole::Other("PSP_S")],
        id: RegisterId(0b00011011),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::Preserve,
    },
    CoreRegister {
        roles: &[
            RegisterRole::Core("MSPLIM_S"),
            RegisterRole::Other("MSPLIM_S"),
        ],
        id: RegisterId(0b00011100),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::SpecialRule,
    },
    CoreRegister {
        roles: &[
            RegisterRole::Core("PSPLIM_S"),
            RegisterRole::Other("PSPLIM_S"),
        ],
        id: RegisterId(0b00011101),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::SpecialRule,
    },
    CoreRegister {
        roles: &[
            RegisterRole::Core("EXTRA_S"),
            RegisterRole::Other("EXTRA_S"),
        ],
        id: RegisterId(0b00100010),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::SpecialRule,
    },
    CoreRegister {
        roles: &[
            RegisterRole::Core("EXTRA_NS"),
            RegisterRole::Other("EXTRA_NS"),
        ],
        id: RegisterId(0b00100010),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule: UnwindRule::SpecialRule,
    },
];

//TODO: VPR (MVE), PACBTI
