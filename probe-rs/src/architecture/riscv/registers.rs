//! RISC-V register descriptions.

use std::sync::LazyLock;

use crate::{
    CoreRegisters,
    core::{CoreRegister, RegisterDataType, RegisterId, RegisterRole, UnwindRule},
};

// ── Width-widening helpers ────────────────────────────────────────────────────

/// Return a copy of `r` with `data_type` changed to `UnsignedInteger(64)`.
pub(crate) const fn as_64bit(r: CoreRegister) -> CoreRegister {
    CoreRegister {
        data_type: RegisterDataType::UnsignedInteger(64),
        ..r
    }
}

/// Return a copy of `r` with `data_type` changed to `FloatingPoint(64)`.
pub(crate) const fn as_64bit_fp(r: CoreRegister) -> CoreRegister {
    CoreRegister {
        data_type: RegisterDataType::FloatingPoint(64),
        ..r
    }
}

// ── Special registers ─────────────────────────────────────────────────────────

/// The program counter register (RV32).
pub const PC: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("pc"), RegisterRole::ProgramCounter],
    id: RegisterId(0x7b1),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

// ── General-purpose registers (RV32) ─────────────────────────────────────────

/// The zero register, x0.
pub const ZERO: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x0"), RegisterRole::Other("zero")],
    id: RegisterId(0x1000),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const RA: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x1"), RegisterRole::ReturnAddress],
    id: RegisterId(0x1001),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const SP: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x2"), RegisterRole::StackPointer],
    id: RegisterId(0x1002),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const GP: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x3"), RegisterRole::Other("gp")],
    id: RegisterId(0x1003),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const TP: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x4"), RegisterRole::Other("tp")],
    id: RegisterId(0x1004),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const T0: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x5"), RegisterRole::Other("t0")],
    id: RegisterId(0x1005),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const T1: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x6"), RegisterRole::Other("t1")],
    id: RegisterId(0x1006),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const T2: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x7"), RegisterRole::Other("t2")],
    id: RegisterId(0x1007),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

/// Frame pointer / first saved register (x8 / s0).
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

// S0 and S1 need to be referenceable as constants in other parts of the architecture specific code.

/// The first saved register, s0. Used as the frame pointer.
pub const S0: CoreRegister = FP;

/// The second saved register, s1.
pub const S1: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x9"), RegisterRole::Other("s1")],
    id: RegisterId(0x1009),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const A0: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("x10"),
        RegisterRole::Argument("a0"),
        RegisterRole::Return("r0"),
    ],
    id: RegisterId(0x100A),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const A1: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("x11"),
        RegisterRole::Argument("a1"),
        RegisterRole::Return("r1"),
    ],
    id: RegisterId(0x100B),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const A2: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x12"), RegisterRole::Argument("a2")],
    id: RegisterId(0x100C),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const A3: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x13"), RegisterRole::Argument("a3")],
    id: RegisterId(0x100D),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const A4: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x14"), RegisterRole::Argument("a4")],
    id: RegisterId(0x100E),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const A5: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x15"), RegisterRole::Argument("a5")],
    id: RegisterId(0x100F),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const A6: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x16"), RegisterRole::Argument("a6")],
    id: RegisterId(0x1010),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const A7: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x17"), RegisterRole::Argument("a7")],
    id: RegisterId(0x1011),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const S2: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x18"), RegisterRole::Other("s2")],
    id: RegisterId(0x1012),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const S3: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x19"), RegisterRole::Other("s3")],
    id: RegisterId(0x1013),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const S4: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x20"), RegisterRole::Other("s4")],
    id: RegisterId(0x1014),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const S5: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x21"), RegisterRole::Other("s5")],
    id: RegisterId(0x1015),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const S6: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x22"), RegisterRole::Other("s6")],
    id: RegisterId(0x1016),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const S7: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x23"), RegisterRole::Other("s7")],
    id: RegisterId(0x1017),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const S8: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x24"), RegisterRole::Other("s8")],
    id: RegisterId(0x1018),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const S9: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x25"), RegisterRole::Other("s9")],
    id: RegisterId(0x1019),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const S10: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x26"), RegisterRole::Other("s10")],
    id: RegisterId(0x101A),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const S11: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x27"), RegisterRole::Other("s11")],
    id: RegisterId(0x101B),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const T3: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x28"), RegisterRole::Other("t3")],
    id: RegisterId(0x101C),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const T4: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x29"), RegisterRole::Other("t4")],
    id: RegisterId(0x101D),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const T5: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x30"), RegisterRole::Other("t5")],
    id: RegisterId(0x101E),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const T6: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("x31"), RegisterRole::Other("t6")],
    id: RegisterId(0x101F),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

// ── Floating-point CSRs (32-bit in both RV32 and RV64) ───────────────────────

pub(crate) const FFLAGS: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("fflags")],
    id: RegisterId(0x001),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FRM: CoreRegister = CoreRegister {
    roles: &[RegisterRole::Core("frm")],
    id: RegisterId(0x002),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FCSR: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("fcsr"),
        RegisterRole::FloatingPointStatus,
    ],
    id: RegisterId(0x003),
    data_type: RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::Clear,
};

// ── Floating-point data registers (RV32 = 32-bit) ────────────────────────────

pub(crate) const FT0: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f0"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("ft0"),
    ],
    id: RegisterId(0x1020),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FT1: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f1"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("ft1"),
    ],
    id: RegisterId(0x1021),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FT2: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f2"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("ft2"),
    ],
    id: RegisterId(0x1022),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FT3: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f3"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("ft3"),
    ],
    id: RegisterId(0x1023),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FT4: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f4"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("ft4"),
    ],
    id: RegisterId(0x1024),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FT5: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f5"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("ft5"),
    ],
    id: RegisterId(0x1025),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FT6: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f6"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("ft6"),
    ],
    id: RegisterId(0x1026),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FT7: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f7"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("ft7"),
    ],
    id: RegisterId(0x1027),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FS0_FP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f8"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fs0"),
    ],
    id: RegisterId(0x1028),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FS1_FP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f9"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fs1"),
    ],
    id: RegisterId(0x1029),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FA0: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f10"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fa0"),
    ],
    id: RegisterId(0x102A),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FA1: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f11"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fa1"),
    ],
    id: RegisterId(0x102B),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FA2: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f12"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fa2"),
    ],
    id: RegisterId(0x102C),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FA3: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f13"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fa3"),
    ],
    id: RegisterId(0x102D),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FA4: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f14"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fa4"),
    ],
    id: RegisterId(0x102E),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FA5: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f15"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fa5"),
    ],
    id: RegisterId(0x102F),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FA6: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f16"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fa6"),
    ],
    id: RegisterId(0x1030),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FA7: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f17"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fa7"),
    ],
    id: RegisterId(0x1031),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FS2_FP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f18"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fs2"),
    ],
    id: RegisterId(0x1032),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FS3_FP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f19"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fs3"),
    ],
    id: RegisterId(0x1033),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FS4_FP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f20"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fs4"),
    ],
    id: RegisterId(0x1034),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FS5_FP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f21"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fs5"),
    ],
    id: RegisterId(0x1035),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FS6_FP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f22"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fs6"),
    ],
    id: RegisterId(0x1036),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FS7_FP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f23"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fs7"),
    ],
    id: RegisterId(0x1037),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FS8_FP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f24"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fs8"),
    ],
    id: RegisterId(0x1038),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FS9_FP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f25"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fs9"),
    ],
    id: RegisterId(0x1039),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FS10_FP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f26"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fs10"),
    ],
    id: RegisterId(0x103A),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FS11_FP: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f27"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("fs11"),
    ],
    id: RegisterId(0x103B),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FT8: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f28"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("ft8"),
    ],
    id: RegisterId(0x103C),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FT9: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f29"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("ft9"),
    ],
    id: RegisterId(0x103D),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FT10: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f30"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("ft10"),
    ],
    id: RegisterId(0x103E),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

pub(crate) const FT11: CoreRegister = CoreRegister {
    roles: &[
        RegisterRole::Core("f31"),
        RegisterRole::FloatingPoint,
        RegisterRole::Other("ft11"),
    ],
    id: RegisterId(0x103F),
    data_type: RegisterDataType::FloatingPoint(32),
    unwind_rule: UnwindRule::Clear,
};

// ── Register sets ─────────────────────────────────────────────────────────────

/// The RISCV core registers without FPU (RV32).
pub static RISCV_CORE_REGISTERS: LazyLock<CoreRegisters> =
    LazyLock::new(|| CoreRegisters::new(RISCV_COMMON_REGS_SET.iter().collect::<Vec<_>>()));

/// The RISCV core registers with FPU (RV32).
pub static RISCV_WITH_FP_CORE_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        RISCV_COMMON_REGS_SET
            .iter()
            .chain(RISCV_WITH_FP_REGS_SET)
            .collect(),
    )
});

// Non-FPU registers
static RISCV_COMMON_REGS_SET: &[CoreRegister] = &[
    ZERO, RA, SP, GP, TP, T0, T1, T2, FP, S1, A0, A1, A2, A3, A4, A5, A6, A7, S2, S3, S4, S5, S6,
    S7, S8, S9, S10, S11, T3, T4, T5, T6, PC,
];

// FPU registers
static RISCV_WITH_FP_REGS_SET: &[CoreRegister] = &[
    FFLAGS, FRM, FCSR, FT0, FT1, FT2, FT3, FT4, FT5, FT6, FT7, FS0_FP, FS1_FP, FA0, FA1, FA2, FA3,
    FA4, FA5, FA6, FA7, FS2_FP, FS3_FP, FS4_FP, FS5_FP, FS6_FP, FS7_FP, FS8_FP, FS9_FP, FS10_FP,
    FS11_FP, FT8, FT9, FT10, FT11,
];
