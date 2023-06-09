use crate::{
    core::{RegisterDataType, RegisterRole},
    CoreRegister, CoreRegisters, RegisterId,
};
use once_cell::sync::Lazy;

pub(crate) const PC: CoreRegister = CoreRegister {
    name: "R15",
    roles: &[RegisterRole::ProgramCounter],
    id: RegisterId(15),
    data_type: RegisterDataType::UnsignedInteger(32),
};

pub(crate) const FP: CoreRegister = CoreRegister {
    name: "R7",
    roles: &[RegisterRole::FramePointer],
    id: RegisterId(7),
    data_type: RegisterDataType::UnsignedInteger(32),
};

pub(crate) const SP: CoreRegister = CoreRegister {
    name: "R13",
    roles: &[RegisterRole::StackPointer],
    id: RegisterId(13),
    data_type: RegisterDataType::UnsignedInteger(32),
};

pub(crate) const RA: CoreRegister = CoreRegister {
    name: "R14",
    roles: &[RegisterRole::ReturnAddress],
    id: RegisterId(14),
    data_type: RegisterDataType::UnsignedInteger(32),
};

pub(crate) static CORTEX_M_CORE_REGSISTERS: Lazy<CoreRegisters> = Lazy::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(CORTEX_M_COMMON_REGS_SET)
            .collect::<Vec<_>>(),
    )
});

pub(crate) static CORTEX_M_WITH_FP_CORE_REGSISTERS: Lazy<CoreRegisters> = Lazy::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(CORTEX_M_COMMON_REGS_SET)
            .chain(CORTEX_M_WITH_FP_REGS_SET)
            .collect(),
    )
});

pub(super) static ARM32_COMMON_REGS_SET: &[CoreRegister] = &[
    CoreRegister {
        name: "R0",
        roles: &[RegisterRole::Argument("a1"), RegisterRole::Return("r1")],
        id: RegisterId(0),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R1",
        roles: &[RegisterRole::Argument("a2"), RegisterRole::Return("r2")],
        id: RegisterId(1),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R2",
        roles: &[RegisterRole::Argument("a3")],
        id: RegisterId(2),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R3",
        roles: &[RegisterRole::Argument("a4")],
        id: RegisterId(3),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R4",
        roles: &[],
        id: RegisterId(4),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R5",
        roles: &[],
        id: RegisterId(5),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R6",
        roles: &[],
        id: RegisterId(6),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    FP,
    CoreRegister {
        name: "R8",
        roles: &[],
        id: RegisterId(8),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R9",
        roles: &[],
        id: RegisterId(9),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R10",
        roles: &[],
        id: RegisterId(10),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R11",
        roles: &[],
        id: RegisterId(11),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R12",
        roles: &[],
        id: RegisterId(12),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    SP,
    RA,
    PC,
];

static CORTEX_M_COMMON_REGS_SET: &[CoreRegister] = &[
    CoreRegister {
        name: "MSP",
        roles: &[RegisterRole::MainStackPointer],
        id: RegisterId(0b10001),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "PSP",
        roles: &[RegisterRole::ProcessStackPointer],
        id: RegisterId(0b10010),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "XPSR",
        roles: &[RegisterRole::ProcessorStatus],
        id: RegisterId(0b1_0000),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    // CONTROL bits [31:24], FAULTMASK bits [23:16],
    // BASEPRI bits [15:8], and PRIMASK bits [7:0]
    CoreRegister {
        name: "EXTRA",
        roles: &[RegisterRole::Other("EXTRA")],
        id: RegisterId(0b10100),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
];

static CORTEX_M_WITH_FP_REGS_SET: &[CoreRegister] = &[
    CoreRegister {
        name: "FPSCR",
        roles: &[RegisterRole::FloatingPointStatus],
        id: RegisterId(33),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "S0",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(64),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S1",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(65),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S2",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(66),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S3",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(67),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S4",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(68),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S5",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(69),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S6",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(70),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S7",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(71),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S8",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(72),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S9",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(73),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S10",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(74),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S11",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(75),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S12",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(76),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S13",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(77),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S14",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(78),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S15",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(79),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S16",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(80),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S17",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(81),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S18",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(82),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S19",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(83),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S20",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(84),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S21",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(85),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S22",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(86),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S23",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(87),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S24",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(88),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S25",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(89),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S26",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(90),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S27",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(91),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S28",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(92),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S29",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(93),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S30",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(94),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S31",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(95),
        data_type: RegisterDataType::FloatingPoint(32),
    },
];
