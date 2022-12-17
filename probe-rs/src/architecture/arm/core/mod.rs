use crate::{
    core::{
        BreakpointCause, MemoryMappedRegister, RegisterDataType, RegisterDescription, RegisterFile,
        RegisterId, RegisterKind, RegisterValue,
    },
    CoreStatus, HaltReason,
};

use bitfield::bitfield;

pub mod armv6m;
pub mod armv7a;
pub mod armv7m;
pub mod armv8a;
pub mod armv8m;

pub(crate) mod armv7a_debug_regs;
pub(crate) mod armv8a_core_regs;
pub(crate) mod armv8a_debug_regs;
pub(crate) mod cortex_m;
pub(crate) mod instructions;

/// Core information data which is downloaded from the target, represents its state and can be used for debugging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dump {
    /// The register values at the time of the dump.
    pub regs: [u32; 16],
    stack_addr: u32,
    stack: Vec<u8>,
}

impl Dump {
    /// Create a new dump from a SP and a stack dump with zeroed out registers.
    pub fn new(stack_addr: u32, stack: Vec<u8>) -> Dump {
        Dump {
            regs: [0u32; 16],
            stack_addr,
            stack,
        }
    }
}

pub(crate) mod register {
    use crate::{
        core::{RegisterDataType, RegisterDescription, RegisterKind},
        RegisterId,
    };

    pub const PC: RegisterDescription = RegisterDescription {
        name: "PC",
        _kind: RegisterKind::PC,
        id: RegisterId(15),
        _type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    };

    pub const CPSR: RegisterDescription = RegisterDescription {
        name: "CPSR",
        _kind: RegisterKind::General,
        id: RegisterId(0b1_0000),
        _type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    };

    pub const XPSR: RegisterDescription = RegisterDescription {
        name: "XPSR",
        _kind: RegisterKind::General,
        id: RegisterId(0b1_0000),
        _type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    };

    pub const SP: RegisterDescription = RegisterDescription {
        name: "SP",
        _kind: RegisterKind::General,
        id: RegisterId(13),
        _type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    };

    pub const LR: RegisterDescription = RegisterDescription {
        name: "LR",
        _kind: RegisterKind::General,
        id: RegisterId(14),
        _type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    };

    pub const MSP: RegisterDescription = RegisterDescription {
        name: "MSP",
        _kind: RegisterKind::General,
        id: RegisterId(0b10001),
        _type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    };

    pub const PSP: RegisterDescription = RegisterDescription {
        name: "PSP",
        _kind: RegisterKind::General,
        id: RegisterId(0b10010),
        _type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    };

    // CONTROL bits [31:24], FAULTMASK bits [23:16],
    // BASEPRI bits [15:8], and PRIMASK bits [7:0]
    pub const EXTRA: RegisterDescription = RegisterDescription {
        name: "EXTRA",
        _kind: RegisterKind::General,
        id: RegisterId(0b10100),
        _type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    };

    pub const FP: RegisterDescription = RegisterDescription {
        name: "FP",
        _kind: RegisterKind::General,
        id: RegisterId(7),
        _type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    };

    pub const FPSCR: RegisterDescription = RegisterDescription {
        name: "FPSCR",
        _kind: RegisterKind::Fp,
        id: RegisterId(33),
        _type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    };

    pub const AARCH32_FPSCR: RegisterDescription = RegisterDescription {
        name: "FPSCR",
        _kind: RegisterKind::Fp,
        id: RegisterId(49),
        _type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    };
}

static ARM32_COMMON_REGS: RegisterFile = RegisterFile {
    platform_registers: &[
        RegisterDescription {
            name: "R0",
            _kind: RegisterKind::General,
            id: RegisterId(0),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R1",
            _kind: RegisterKind::General,
            id: RegisterId(1),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R2",
            _kind: RegisterKind::General,
            id: RegisterId(2),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R3",
            _kind: RegisterKind::General,
            id: RegisterId(3),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R4",
            _kind: RegisterKind::General,
            id: RegisterId(4),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R5",
            _kind: RegisterKind::General,
            id: RegisterId(5),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R6",
            _kind: RegisterKind::General,
            id: RegisterId(6),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R7",
            _kind: RegisterKind::General,
            id: RegisterId(7),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R8",
            _kind: RegisterKind::General,
            id: RegisterId(8),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R9",
            _kind: RegisterKind::General,
            id: RegisterId(9),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R10",
            _kind: RegisterKind::General,
            id: RegisterId(10),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R11",
            _kind: RegisterKind::General,
            id: RegisterId(11),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R12",
            _kind: RegisterKind::General,
            id: RegisterId(12),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R13",
            _kind: RegisterKind::General,
            id: RegisterId(13),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R14",
            _kind: RegisterKind::General,
            id: RegisterId(14),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "R15",
            _kind: RegisterKind::General,
            id: RegisterId(15),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
    ],

    program_counter: &register::PC,
    stack_pointer: &register::SP,
    return_address: &register::LR,
    frame_pointer: &register::FP,

    argument_registers: &[
        RegisterDescription {
            name: "a1",
            _kind: RegisterKind::General,
            id: RegisterId(0),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "a2",
            _kind: RegisterKind::General,
            id: RegisterId(1),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "a3",
            _kind: RegisterKind::General,
            id: RegisterId(2),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "a4",
            _kind: RegisterKind::General,
            id: RegisterId(3),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
    ],

    result_registers: &[
        RegisterDescription {
            name: "a1",
            _kind: RegisterKind::General,
            id: RegisterId(0),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "a2",
            _kind: RegisterKind::General,
            id: RegisterId(1),
            _type: RegisterDataType::UnsignedInteger,
            size_in_bits: 32,
        },
    ],

    msp: None,
    psp: None,
    other: &[],
    psr: None,

    fp_status: None,
    fp_registers: None,
};

static AARCH32_COMMON_REGS: RegisterFile = RegisterFile {
    psr: Some(&register::CPSR),

    ..ARM32_COMMON_REGS
};

static AARCH32_FP_16_REGS: RegisterFile = RegisterFile {
    fp_status: Some(&register::AARCH32_FPSCR),
    fp_registers: Some(&[
        RegisterDescription {
            name: "D0",
            _kind: RegisterKind::Fp,
            id: RegisterId(17),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D1",
            _kind: RegisterKind::Fp,
            id: RegisterId(18),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D2",
            _kind: RegisterKind::Fp,
            id: RegisterId(19),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D3",
            _kind: RegisterKind::Fp,
            id: RegisterId(20),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D4",
            _kind: RegisterKind::Fp,
            id: RegisterId(21),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D5",
            _kind: RegisterKind::Fp,
            id: RegisterId(22),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D6",
            _kind: RegisterKind::Fp,
            id: RegisterId(23),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D7",
            _kind: RegisterKind::Fp,
            id: RegisterId(24),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D8",
            _kind: RegisterKind::Fp,
            id: RegisterId(25),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D9",
            _kind: RegisterKind::Fp,
            id: RegisterId(26),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D10",
            _kind: RegisterKind::Fp,
            id: RegisterId(27),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D11",
            _kind: RegisterKind::Fp,
            id: RegisterId(28),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D12",
            _kind: RegisterKind::Fp,
            id: RegisterId(29),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D13",
            _kind: RegisterKind::Fp,
            id: RegisterId(30),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D14",
            _kind: RegisterKind::Fp,
            id: RegisterId(31),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D15",
            _kind: RegisterKind::Fp,
            id: RegisterId(32),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
    ]),

    ..AARCH32_COMMON_REGS
};

static AARCH32_FP_32_REGS: RegisterFile = RegisterFile {
    fp_status: Some(&register::AARCH32_FPSCR),
    fp_registers: Some(&[
        RegisterDescription {
            name: "D0",
            _kind: RegisterKind::Fp,
            id: RegisterId(17),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D1",
            _kind: RegisterKind::Fp,
            id: RegisterId(18),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D2",
            _kind: RegisterKind::Fp,
            id: RegisterId(19),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D3",
            _kind: RegisterKind::Fp,
            id: RegisterId(20),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D4",
            _kind: RegisterKind::Fp,
            id: RegisterId(21),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D5",
            _kind: RegisterKind::Fp,
            id: RegisterId(22),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D6",
            _kind: RegisterKind::Fp,
            id: RegisterId(23),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D7",
            _kind: RegisterKind::Fp,
            id: RegisterId(24),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D8",
            _kind: RegisterKind::Fp,
            id: RegisterId(25),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D9",
            _kind: RegisterKind::Fp,
            id: RegisterId(26),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D10",
            _kind: RegisterKind::Fp,
            id: RegisterId(27),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D11",
            _kind: RegisterKind::Fp,
            id: RegisterId(28),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D12",
            _kind: RegisterKind::Fp,
            id: RegisterId(29),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D13",
            _kind: RegisterKind::Fp,
            id: RegisterId(30),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D14",
            _kind: RegisterKind::Fp,
            id: RegisterId(31),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D15",
            _kind: RegisterKind::Fp,
            id: RegisterId(32),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D16",
            _kind: RegisterKind::Fp,
            id: RegisterId(33),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D17",
            _kind: RegisterKind::Fp,
            id: RegisterId(34),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D18",
            _kind: RegisterKind::Fp,
            id: RegisterId(35),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D19",
            _kind: RegisterKind::Fp,
            id: RegisterId(36),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D20",
            _kind: RegisterKind::Fp,
            id: RegisterId(37),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D21",
            _kind: RegisterKind::Fp,
            id: RegisterId(38),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D22",
            _kind: RegisterKind::Fp,
            id: RegisterId(39),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D23",
            _kind: RegisterKind::Fp,
            id: RegisterId(40),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D24",
            _kind: RegisterKind::Fp,
            id: RegisterId(41),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D25",
            _kind: RegisterKind::Fp,
            id: RegisterId(42),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D26",
            _kind: RegisterKind::Fp,
            id: RegisterId(43),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D27",
            _kind: RegisterKind::Fp,
            id: RegisterId(44),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D28",
            _kind: RegisterKind::Fp,
            id: RegisterId(45),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D29",
            _kind: RegisterKind::Fp,
            id: RegisterId(46),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D30",
            _kind: RegisterKind::Fp,
            id: RegisterId(47),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
        RegisterDescription {
            name: "D31",
            _kind: RegisterKind::Fp,
            id: RegisterId(48),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 64,
        },
    ]),

    ..AARCH32_COMMON_REGS
};

static CORTEX_M_COMMON_REGS: RegisterFile = RegisterFile {
    msp: Some(&register::MSP),
    psp: Some(&register::PSP),
    other: &[register::EXTRA],
    psr: Some(&register::XPSR),

    ..ARM32_COMMON_REGS
};

static CORTEX_M_WITH_FP_REGS: RegisterFile = RegisterFile {
    fp_status: Some(&register::FPSCR),
    fp_registers: Some(&[
        RegisterDescription {
            name: "S0",
            _kind: RegisterKind::Fp,
            id: RegisterId(64),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S1",
            _kind: RegisterKind::Fp,
            id: RegisterId(65),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S2",
            _kind: RegisterKind::Fp,
            id: RegisterId(66),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S3",
            _kind: RegisterKind::Fp,
            id: RegisterId(67),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S4",
            _kind: RegisterKind::Fp,
            id: RegisterId(68),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S5",
            _kind: RegisterKind::Fp,
            id: RegisterId(69),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S6",
            _kind: RegisterKind::Fp,
            id: RegisterId(70),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S7",
            _kind: RegisterKind::Fp,
            id: RegisterId(71),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S8",
            _kind: RegisterKind::Fp,
            id: RegisterId(72),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S9",
            _kind: RegisterKind::Fp,
            id: RegisterId(73),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S10",
            _kind: RegisterKind::Fp,
            id: RegisterId(74),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S11",
            _kind: RegisterKind::Fp,
            id: RegisterId(75),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S12",
            _kind: RegisterKind::Fp,
            id: RegisterId(76),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S13",
            _kind: RegisterKind::Fp,
            id: RegisterId(77),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S14",
            _kind: RegisterKind::Fp,
            id: RegisterId(78),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S15",
            _kind: RegisterKind::Fp,
            id: RegisterId(79),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S16",
            _kind: RegisterKind::Fp,
            id: RegisterId(80),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S17",
            _kind: RegisterKind::Fp,
            id: RegisterId(81),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S18",
            _kind: RegisterKind::Fp,
            id: RegisterId(82),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S19",
            _kind: RegisterKind::Fp,
            id: RegisterId(83),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S20",
            _kind: RegisterKind::Fp,
            id: RegisterId(84),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S21",
            _kind: RegisterKind::Fp,
            id: RegisterId(85),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S22",
            _kind: RegisterKind::Fp,
            id: RegisterId(86),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S23",
            _kind: RegisterKind::Fp,
            id: RegisterId(87),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S24",
            _kind: RegisterKind::Fp,
            id: RegisterId(88),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S25",
            _kind: RegisterKind::Fp,
            id: RegisterId(89),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S26",
            _kind: RegisterKind::Fp,
            id: RegisterId(90),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S27",
            _kind: RegisterKind::Fp,
            id: RegisterId(91),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S28",
            _kind: RegisterKind::Fp,
            id: RegisterId(92),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S29",
            _kind: RegisterKind::Fp,
            id: RegisterId(93),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S30",
            _kind: RegisterKind::Fp,
            id: RegisterId(94),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
        RegisterDescription {
            name: "S31",
            _kind: RegisterKind::Fp,
            id: RegisterId(95),
            _type: RegisterDataType::FloatingPoint,
            size_in_bits: 32,
        },
    ]),

    ..CORTEX_M_COMMON_REGS
};

bitfield! {
    #[derive(Copy, Clone)]
    pub struct Dfsr(u32);
    impl Debug;
    pub external, set_external: 4;
    pub vcatch, set_vcatch: 3;
    pub dwttrap, set_dwttrap: 2;
    pub bkpt, set_bkpt: 1;
    pub halted, set_halted: 0;
}

impl Dfsr {
    fn clear_all() -> Self {
        Dfsr(0b11111)
    }

    /// This only returns the correct halt_reason for armv(x)-m variants. The armv(x)-a variants have their own implementation.
    // TODO: The different implementations between -m and -a can do with cleanup/refactoring.
    fn halt_reason(&self) -> HaltReason {
        if self.0 == 0 {
            // No bit is set
            HaltReason::Unknown
        } else if self.0.count_ones() > 1 {
            tracing::debug!("DFSR: {:?}", self);

            // We cannot identify why the chip halted,
            // it could be for multiple reasons.

            // For debuggers, it's important to know if
            // the core halted because of a breakpoint.
            // Because of this, we still return breakpoint
            // even if other reasons are possible as well.
            if self.bkpt() {
                HaltReason::Breakpoint(BreakpointCause::Unknown)
            } else {
                HaltReason::Multiple
            }
        } else if self.bkpt() {
            HaltReason::Breakpoint(BreakpointCause::Unknown)
        } else if self.external() {
            HaltReason::External
        } else if self.dwttrap() {
            HaltReason::Watchpoint
        } else if self.halted() {
            HaltReason::Request
        } else if self.vcatch() {
            HaltReason::Exception
        } else {
            // We check that exactly one bit is set, so we should hit one of the cases above.
            panic!("This should not happen. Please open a bug report.")
        }
    }
}

impl From<u32> for Dfsr {
    fn from(val: u32) -> Self {
        // Ensure that all unused bits are set to zero
        // This makes it possible to check the number of
        // set bits using count_ones().
        Dfsr(val & 0b11111)
    }
}

impl From<Dfsr> for u32 {
    fn from(register: Dfsr) -> Self {
        register.0
    }
}

impl MemoryMappedRegister for Dfsr {
    const ADDRESS: u64 = 0xE000_ED30;
    const NAME: &'static str = "DFSR";
}

#[derive(Debug)]
pub struct CortexMState {
    initialized: bool,

    hw_breakpoints_enabled: bool,

    current_state: CoreStatus,

    fp_present: bool,
}

impl CortexMState {
    pub(crate) fn new() -> Self {
        Self {
            initialized: false,
            hw_breakpoints_enabled: false,
            current_state: CoreStatus::Unknown,
            fp_present: false,
        }
    }

    fn initialize(&mut self) {
        self.initialized = true;
    }

    fn initialized(&self) -> bool {
        self.initialized
    }
}

#[derive(Debug)]
pub struct CortexAState {
    initialized: bool,

    current_state: CoreStatus,

    // Is the core currently in a 64-bit mode?
    is_64_bit: bool,

    register_cache: Vec<Option<(RegisterValue, bool)>>,

    // Number of floating point registers
    fp_reg_count: Option<usize>,
}

impl CortexAState {
    pub(crate) fn new() -> Self {
        Self {
            initialized: false,
            current_state: CoreStatus::Unknown,
            is_64_bit: false,
            register_cache: vec![],
            fp_reg_count: None,
        }
    }

    fn initialize(&mut self) {
        self.initialized = true;
    }

    fn initialized(&self) -> bool {
        self.initialized
    }
}
