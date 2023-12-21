use crate::{
    core::{RegisterDataType, UnwindRule},
    CoreRegister, CoreRegisters, RegisterRole,
};
use once_cell::sync::Lazy;

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

pub(crate) static XTENSA_CORE_REGSISTERS: Lazy<CoreRegisters> =
    Lazy::new(|| CoreRegisters::new(XTENSA_REGISTERS_SET.iter().collect()));

static XTENSA_REGISTERS_SET: &[CoreRegister] = &[RA, PC, SP, FP];
