//! ARM7TDMI register definitions

use crate::core::{
    CoreRegister, CoreRegisters, RegisterDataType, RegisterId, RegisterRole, UnwindRule,
};
use std::sync::LazyLock;

/// Program Counter (R15)
pub const PC: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("PC"), RegisterRole::ProgramCounter],
    id: RegisterId(15),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::SpecialRule,
};

/// Link Register (R14)
pub const LR: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("LR"), RegisterRole::ReturnAddress],
    id: RegisterId(14),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Preserve,
};

/// Stack Pointer (R13)
pub const SP: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("SP"), RegisterRole::StackPointer],
    id: RegisterId(13),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::SpecialRule,
};

/// Frame Pointer (R11)
pub const FP: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R11"), RegisterRole::FramePointer],
    id: RegisterId(11),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Preserve,
};

/// Current Program Status Register
pub const CPSR: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("CPSR"), RegisterRole::ProcessorStatus],
    id: RegisterId(16),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::SpecialRule,
};

/// General purpose register R0
pub const R0: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("R0"),
        RegisterRole::Argument("a1"),
        RegisterRole::Return("r1"),
    ],
    id: RegisterId(0),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

/// General purpose register R1
pub const R1: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R1"), RegisterRole::Argument("a2")],
    id: RegisterId(1),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

/// General purpose register R2
pub const R2: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R2"), RegisterRole::Argument("a3")],
    id: RegisterId(2),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

/// General purpose register R3
pub const R3: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R3"), RegisterRole::Argument("a4")],
    id: RegisterId(3),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

/// General purpose register R4
pub const R4: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R4")],
    id: RegisterId(4),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Preserve,
};

/// General purpose register R5
pub const R5: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R5")],
    id: RegisterId(5),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Preserve,
};

/// General purpose register R6
pub const R6: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R6")],
    id: RegisterId(6),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Preserve,
};

/// General purpose register R7
pub const R7: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R7")],
    id: RegisterId(7),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Preserve,
};

/// General purpose register R8
pub const R8: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R8")],
    id: RegisterId(8),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Preserve,
};

/// General purpose register R9
pub const R9: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R9")],
    id: RegisterId(9),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Preserve,
};

/// General purpose register R10
pub const R10: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R10")],
    id: RegisterId(10),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Preserve,
};

/// General purpose register R12
pub const R12: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("R12")],
    id: RegisterId(12),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

/// All ARM7TDMI core registers
static ARM7TDMI_CORE_REGS_SET: &[CoreRegister] = &[
    R0, R1, R2, R3, R4, R5, R6, R7, R8, R9, R10, FP, R12, SP, LR, PC, CPSR,
];

/// The ARM7TDMI core registers
pub static ARM7TDMI_CORE_REGISTERS: LazyLock<CoreRegisters> =
    LazyLock::new(|| CoreRegisters::new(ARM7TDMI_CORE_REGS_SET.iter().collect::<Vec<_>>()));
