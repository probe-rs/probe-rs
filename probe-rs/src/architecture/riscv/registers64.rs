//! RISC-V 64-bit register descriptions.
//!
//! All constants are derived from their 32-bit counterparts in [`super::registers`]
//! via `registers::as_64bit` / `registers::as_64bit_fp`, keeping the register
//! IDs and roles identical while widening the data type to 64 bits.

use std::sync::LazyLock;

use super::registers::{
    self,
    A0,
    A1,
    A2,
    A3,
    A4,
    A5,
    A6,
    A7,
    FA0,
    FA1,
    FA2,
    FA3,
    FA4,
    FA5,
    FA6,
    FA7,
    FCSR,
    // FP CSRs (stay 32-bit on RV64 too)
    FFLAGS,
    FP,
    FRM,
    FS0_FP,
    FS1_FP,
    FS2_FP,
    FS3_FP,
    FS4_FP,
    FS5_FP,
    FS6_FP,
    FS7_FP,
    FS8_FP,
    FS9_FP,
    FS10_FP,
    FS11_FP,
    // FP data registers
    FT0,
    FT1,
    FT2,
    FT3,
    FT4,
    FT5,
    FT6,
    FT7,
    FT8,
    FT9,
    FT10,
    FT11,
    GP,
    // Integer registers
    PC,
    RA,
    S1,
    S2,
    S3,
    S4,
    S5,
    S6,
    S7,
    S8,
    S9,
    S10,
    S11,
    SP,
    T0,
    T1,
    T2,
    T3,
    T4,
    T5,
    T6,
    TP,
    ZERO,
};
use crate::CoreRegisters;
use crate::core::CoreRegister;

// ── Named 64-bit constants (used by mod.rs) ───────────────────────────────────

/// The program counter register (RV64).
pub const PC64: CoreRegister = registers::as_64bit(PC);
pub(crate) const FP64: CoreRegister = registers::as_64bit(FP);
pub(crate) const SP64: CoreRegister = registers::as_64bit(SP);
pub(crate) const RA64: CoreRegister = registers::as_64bit(RA);
/// The zero register, x0 (RV64).
pub const ZERO64: CoreRegister = registers::as_64bit(ZERO);
/// The first saved register, s0. Used as the frame pointer (RV64).
pub const S0_64: CoreRegister = FP64;
/// The second saved register, s1 (RV64).
pub const S1_64: CoreRegister = registers::as_64bit(S1);

// ── Register sets ─────────────────────────────────────────────────────────────

/// The RISCV core registers without FPU (RV64).
pub static RISCV64_CORE_REGISTERS: LazyLock<CoreRegisters> =
    LazyLock::new(|| CoreRegisters::new(RISCV64_COMMON_REGS_SET.iter().collect::<Vec<_>>()));

/// The RISCV core registers with FPU (RV64, double-precision).
pub static RISCV64_WITH_FP_CORE_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(
        RISCV64_COMMON_REGS_SET
            .iter()
            .chain(RISCV64_WITH_FP_REGS_SET)
            .collect(),
    )
});

static RISCV64_COMMON_REGS_SET: &[CoreRegister] = &[
    registers::as_64bit(ZERO),
    registers::as_64bit(RA),
    registers::as_64bit(SP),
    registers::as_64bit(GP),
    registers::as_64bit(TP),
    registers::as_64bit(T0),
    registers::as_64bit(T1),
    registers::as_64bit(T2),
    registers::as_64bit(FP),
    registers::as_64bit(S1),
    registers::as_64bit(A0),
    registers::as_64bit(A1),
    registers::as_64bit(A2),
    registers::as_64bit(A3),
    registers::as_64bit(A4),
    registers::as_64bit(A5),
    registers::as_64bit(A6),
    registers::as_64bit(A7),
    registers::as_64bit(S2),
    registers::as_64bit(S3),
    registers::as_64bit(S4),
    registers::as_64bit(S5),
    registers::as_64bit(S6),
    registers::as_64bit(S7),
    registers::as_64bit(S8),
    registers::as_64bit(S9),
    registers::as_64bit(S10),
    registers::as_64bit(S11),
    registers::as_64bit(T3),
    registers::as_64bit(T4),
    registers::as_64bit(T5),
    registers::as_64bit(T6),
    PC64,
];

static RISCV64_WITH_FP_REGS_SET: &[CoreRegister] = &[
    // FP CSRs are 32-bit in both RV32 and RV64.
    FFLAGS,
    FRM,
    FCSR,
    // FP data registers widen to 64-bit on RV64.
    registers::as_64bit_fp(FT0),
    registers::as_64bit_fp(FT1),
    registers::as_64bit_fp(FT2),
    registers::as_64bit_fp(FT3),
    registers::as_64bit_fp(FT4),
    registers::as_64bit_fp(FT5),
    registers::as_64bit_fp(FT6),
    registers::as_64bit_fp(FT7),
    registers::as_64bit_fp(FS0_FP),
    registers::as_64bit_fp(FS1_FP),
    registers::as_64bit_fp(FA0),
    registers::as_64bit_fp(FA1),
    registers::as_64bit_fp(FA2),
    registers::as_64bit_fp(FA3),
    registers::as_64bit_fp(FA4),
    registers::as_64bit_fp(FA5),
    registers::as_64bit_fp(FA6),
    registers::as_64bit_fp(FA7),
    registers::as_64bit_fp(FS2_FP),
    registers::as_64bit_fp(FS3_FP),
    registers::as_64bit_fp(FS4_FP),
    registers::as_64bit_fp(FS5_FP),
    registers::as_64bit_fp(FS6_FP),
    registers::as_64bit_fp(FS7_FP),
    registers::as_64bit_fp(FS8_FP),
    registers::as_64bit_fp(FS9_FP),
    registers::as_64bit_fp(FS10_FP),
    registers::as_64bit_fp(FS11_FP),
    registers::as_64bit_fp(FT8),
    registers::as_64bit_fp(FT9),
    registers::as_64bit_fp(FT10),
    registers::as_64bit_fp(FT11),
];
