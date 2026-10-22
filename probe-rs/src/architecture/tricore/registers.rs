//! TriCore register descriptions.
//!
//! Every register is memory mapped in the CSFR window of its core. The [`RegisterId`] of a
//! register is its offset in that window, so reading a register is one memory read at
//! `csfr_base + id`. The DWARF numbers follow the TriCore EABI.
//!
//! `CALL` saves the upper context (`A10`-`A15`, `D8`-`D15`, `PSW`, `PCXI`) of the caller into a
//! context save area (CSA) in hardware, so compilers emit no unwind rules for these registers.
//! They are marked [`UnwindRule::SpecialRule`]: the unwinder restores them from the CSA.

use std::sync::LazyLock;

use crate::{
    CoreRegister, CoreRegisters, RegisterId, RegisterRole,
    core::{RegisterDataType, UnwindRule},
};

/// CSFR offset of `PCXI` (previous context information).
pub const CSFR_PCXI: u16 = 0xFE00;
/// CSFR offset of `PSW` (program status word).
pub const CSFR_PSW: u16 = 0xFE04;
/// CSFR offset of `PC` (program counter).
pub const CSFR_PC: u16 = 0xFE08;
/// CSFR offset of `BIV` (interrupt vector table base).
pub const CSFR_BIV: u16 = 0xFE20;
/// CSFR offset of `BTV` (trap vector table base).
pub const CSFR_BTV: u16 = 0xFE24;
/// CSFR offset of `ISP` (interrupt stack pointer).
pub const CSFR_ISP: u16 = 0xFE28;
/// CSFR offset of `ICR` (interrupt control register).
pub const CSFR_ICR: u16 = 0xFE2C;
/// CSFR offset of `FCX` (free CSA list head).
pub const CSFR_FCX: u16 = 0xFE38;
/// CSFR offset of `D0`. `Dn` is at `CSFR_D0 + 4 * n`.
pub const CSFR_D0: u16 = 0xFF00;
/// CSFR offset of `A0`. `An` is at `CSFR_A0 + 4 * n`.
pub const CSFR_A0: u16 = 0xFF80;

/// DWARF number of `PSW`.
pub const DWARF_PSW: u16 = 40;
/// DWARF number of `PCXI`.
pub const DWARF_PCXI: u16 = 41;
/// DWARF number of `PC`.
pub const DWARF_PC: u16 = 42;
/// DWARF number of `FCX`.
pub const DWARF_FCX: u16 = 43;
/// DWARF number of `D0`. `Dn` is `n`.
pub const DWARF_D0: u16 = 0;
/// DWARF number of `A0`. `An` is `16 + n`.
pub const DWARF_A0: u16 = 16;

const fn reg(
    roles: &'static [RegisterRole],
    id: u16,
    dwarf_id: u16,
    unwind_rule: UnwindRule,
) -> CoreRegister {
    CoreRegister {
        roles,
        id: RegisterId(id),
        dwarf_id: Some(dwarf_id),
        data_type: RegisterDataType::UnsignedInteger(32),
        unwind_rule,
    }
}

const fn d(n: u16, roles: &'static [RegisterRole], unwind_rule: UnwindRule) -> CoreRegister {
    reg(roles, CSFR_D0 + 4 * n, DWARF_D0 + n, unwind_rule)
}

const fn a(n: u16, roles: &'static [RegisterRole], unwind_rule: UnwindRule) -> CoreRegister {
    reg(roles, CSFR_A0 + 4 * n, DWARF_A0 + n, unwind_rule)
}

/// The program counter.
pub const PC: CoreRegister = reg(
    &[RegisterRole::Core("pc"), RegisterRole::ProgramCounter],
    CSFR_PC,
    DWARF_PC,
    UnwindRule::SpecialRule,
);

/// The stack pointer (`A10`).
pub const SP: CoreRegister = a(
    10,
    &[
        RegisterRole::Core("a10"),
        RegisterRole::Core("sp"),
        RegisterRole::StackPointer,
    ],
    UnwindRule::SpecialRule,
);

/// The return address (`A11`).
pub const RA: CoreRegister = a(
    11,
    &[
        RegisterRole::Core("a11"),
        RegisterRole::Core("ra"),
        RegisterRole::ReturnAddress,
    ],
    UnwindRule::SpecialRule,
);

/// The frame pointer (`A14`, as used by GCC).
pub const FP: CoreRegister = a(
    14,
    &[
        RegisterRole::Core("a14"),
        RegisterRole::Core("fp"),
        RegisterRole::FramePointer,
    ],
    UnwindRule::SpecialRule,
);

/// The program status word.
pub const PSW: CoreRegister = reg(
    &[RegisterRole::Core("psw"), RegisterRole::ProcessorStatus],
    CSFR_PSW,
    DWARF_PSW,
    UnwindRule::SpecialRule,
);

/// The previous context information: the link to the CSA of the calling context.
pub const PCXI: CoreRegister = reg(
    &[RegisterRole::Core("pcxi")],
    CSFR_PCXI,
    DWARF_PCXI,
    UnwindRule::SpecialRule,
);

/// The head of the free CSA list.
pub const FCX: CoreRegister = reg(
    &[RegisterRole::Core("fcx")],
    CSFR_FCX,
    DWARF_FCX,
    UnwindRule::Preserve,
);

/// The interrupt control register.
pub const ICR: CoreRegister = reg(
    &[RegisterRole::Core("icr")],
    CSFR_ICR,
    46,
    UnwindRule::Preserve,
);

use UnwindRule::{Clear, Preserve, SpecialRule};

static TRICORE_REGISTERS_SET: &[CoreRegister] = &[
    PC,
    // Lower context: caller saved.
    d(0, &[RegisterRole::Core("d0")], Clear),
    d(1, &[RegisterRole::Core("d1")], Clear),
    d(
        2,
        &[RegisterRole::Core("d2"), RegisterRole::Return("d2")],
        Clear,
    ),
    d(
        3,
        &[RegisterRole::Core("d3"), RegisterRole::Return("d3")],
        Clear,
    ),
    d(
        4,
        &[RegisterRole::Core("d4"), RegisterRole::Argument("d4")],
        Clear,
    ),
    d(
        5,
        &[RegisterRole::Core("d5"), RegisterRole::Argument("d5")],
        Clear,
    ),
    d(
        6,
        &[RegisterRole::Core("d6"), RegisterRole::Argument("d6")],
        Clear,
    ),
    d(
        7,
        &[RegisterRole::Core("d7"), RegisterRole::Argument("d7")],
        Clear,
    ),
    // Upper context: saved into the CSA by CALL.
    d(8, &[RegisterRole::Core("d8")], SpecialRule),
    d(9, &[RegisterRole::Core("d9")], SpecialRule),
    d(10, &[RegisterRole::Core("d10")], SpecialRule),
    d(11, &[RegisterRole::Core("d11")], SpecialRule),
    d(12, &[RegisterRole::Core("d12")], SpecialRule),
    d(13, &[RegisterRole::Core("d13")], SpecialRule),
    d(14, &[RegisterRole::Core("d14")], SpecialRule),
    d(15, &[RegisterRole::Core("d15")], SpecialRule),
    // A0, A1, A8 and A9 are global registers: no function changes them.
    a(0, &[RegisterRole::Core("a0")], Preserve),
    a(1, &[RegisterRole::Core("a1")], Preserve),
    a(
        2,
        &[RegisterRole::Core("a2"), RegisterRole::Return("a2")],
        Clear,
    ),
    a(3, &[RegisterRole::Core("a3")], Clear),
    a(
        4,
        &[RegisterRole::Core("a4"), RegisterRole::Argument("a4")],
        Clear,
    ),
    a(
        5,
        &[RegisterRole::Core("a5"), RegisterRole::Argument("a5")],
        Clear,
    ),
    a(
        6,
        &[RegisterRole::Core("a6"), RegisterRole::Argument("a6")],
        Clear,
    ),
    a(
        7,
        &[RegisterRole::Core("a7"), RegisterRole::Argument("a7")],
        Clear,
    ),
    a(8, &[RegisterRole::Core("a8")], Preserve),
    a(9, &[RegisterRole::Core("a9")], Preserve),
    SP,
    RA,
    a(12, &[RegisterRole::Core("a12")], SpecialRule),
    a(13, &[RegisterRole::Core("a13")], SpecialRule),
    FP,
    a(15, &[RegisterRole::Core("a15")], SpecialRule),
    PSW,
    PCXI,
    FCX,
    ICR,
    reg(&[RegisterRole::Core("isp")], CSFR_ISP, 45, Preserve),
    reg(&[RegisterRole::Core("biv")], CSFR_BIV, 48, Preserve),
    reg(&[RegisterRole::Core("btv")], CSFR_BTV, 49, Preserve),
];

/// The registers of a TriCore core.
pub static TRICORE_CORE_REGISTERS: LazyLock<CoreRegisters> =
    LazyLock::new(|| CoreRegisters::new(TRICORE_REGISTERS_SET.iter().collect()));

/// The word index in a context save area for each upper context register, by DWARF number.
///
/// The upper context CSA holds, in order: `PCXI`, `PSW`, `A10`, `A11`, `D8`-`D11`, `A12`-`A15`
/// and `D12`-`D15`.
pub fn upper_context_word(dwarf_id: u16) -> Option<usize> {
    Some(match dwarf_id {
        DWARF_PCXI => 0,
        DWARF_PSW => 1,
        26 => 2,                                  // A10
        27 => 3,                                  // A11
        8..=11 => 4 + (dwarf_id - 8) as usize,    // D8-D11
        28..=31 => 8 + (dwarf_id - 28) as usize,  // A12-A15
        12..=15 => 12 + (dwarf_id - 12) as usize, // D12-D15
        _ => return None,
    })
}

/// The address of the context save area that a `PCXI` or `FCX` value links to.
///
/// `PCXI[19:16]` holds the segment (address bits 31:28) and `PCXI[15:0]` the offset (address
/// bits 21:6).
pub fn csa_address(link: u32) -> u32 {
    ((link & 0xF_0000) << 12) | ((link & 0xFFFF) << 6)
}

/// Whether a `PCXI` value links to an upper context (`PCXI.UL`). TC1.6 keeps this flag in bit 20.
pub fn pcxi_links_upper_context(pcxi: u32) -> bool {
    pcxi & (1 << 20) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csa_address_decodes_segment_and_offset() {
        // A CSA in the DSPR of CPU0 (0x7000_xxxx global alias of 0xD000_xxxx).
        assert_eq!(csa_address(0x000D_0240), 0xD000_9000);
        assert_eq!(csa_address(0x0007_0001), 0x7000_0040);
        assert_eq!(csa_address(0), 0);
    }

    #[test]
    fn upper_context_layout() {
        assert_eq!(upper_context_word(DWARF_PCXI), Some(0));
        assert_eq!(upper_context_word(DWARF_PSW), Some(1));
        assert_eq!(upper_context_word(26), Some(2));
        assert_eq!(upper_context_word(27), Some(3));
        assert_eq!(upper_context_word(8), Some(4));
        assert_eq!(upper_context_word(11), Some(7));
        assert_eq!(upper_context_word(28), Some(8));
        assert_eq!(upper_context_word(31), Some(11));
        assert_eq!(upper_context_word(12), Some(12));
        assert_eq!(upper_context_word(15), Some(15));
        assert_eq!(upper_context_word(4), None);
        assert_eq!(upper_context_word(DWARF_PC), None);
    }

    #[test]
    fn register_ids_are_csfr_offsets() {
        assert_eq!(SP.id, RegisterId(0xFFA8));
        assert_eq!(RA.id, RegisterId(0xFFAC));
        assert_eq!(FP.id, RegisterId(0xFFB8));
        // PC, D0-D15, A0-A15, PSW, PCXI, FCX, ICR, ISP, BIV, BTV.
        assert_eq!(TRICORE_CORE_REGISTERS.all_registers().count(), 40);
    }
}
