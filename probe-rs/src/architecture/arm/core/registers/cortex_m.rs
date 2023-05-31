use crate::{
    core::{RegisterDataType, RegisterRole},
    CoreRegister, RegisterFile, RegisterId,
};
use once_cell::sync::Lazy;

pub(crate) static CORTEX_M_REGISTER_FILE: Lazy<RegisterFile> = Lazy::new(|| {
    RegisterFile::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(CORTEX_M_COMMON_REGS_SET)
            .collect::<Vec<_>>(),
    )
});

pub(crate) static CORTEX_M_WITH_FP_REGISTER_FILE: Lazy<RegisterFile> = Lazy::new(|| {
    RegisterFile::new(
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
        role: Some(RegisterRole::ArgumentAndResult("a1")),
        id: RegisterId(0),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R1",
        role: Some(RegisterRole::ArgumentAndResult("a2")),
        id: RegisterId(1),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R2",
        role: Some(RegisterRole::Argument("a3")),
        id: RegisterId(2),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R3",
        role: Some(RegisterRole::Argument("a4")),
        id: RegisterId(3),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R4",
        role: None,
        id: RegisterId(4),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R5",
        role: None,
        id: RegisterId(5),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R6",
        role: None,
        id: RegisterId(6),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R7",
        role: Some(RegisterRole::FramePointer),
        id: RegisterId(7),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R8",
        role: None,
        id: RegisterId(8),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R9",
        role: None,
        id: RegisterId(9),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R10",
        role: None,
        id: RegisterId(10),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R11",
        role: None,
        id: RegisterId(11),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R12",
        role: None,
        id: RegisterId(12),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R13",
        role: Some(RegisterRole::StackPointer),
        id: RegisterId(13),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R14",
        role: Some(RegisterRole::ReturnAddress),
        id: RegisterId(14),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "R15",
        role: Some(RegisterRole::ProgramCounter),
        id: RegisterId(15),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
];

static CORTEX_M_COMMON_REGS_SET: &[CoreRegister] = &[
    CoreRegister {
        name: "MSP",
        role: Some(RegisterRole::MainStackPointer),
        id: RegisterId(0b10001),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "PSP",
        role: Some(RegisterRole::ProcessStackPointer),
        id: RegisterId(0b10010),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "XPSR",
        role: Some(RegisterRole::ProcessorStatus),
        id: RegisterId(0b1_0000),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    // CONTROL bits [31:24], FAULTMASK bits [23:16],
    // BASEPRI bits [15:8], and PRIMASK bits [7:0]
    CoreRegister {
        name: "EXTRA",
        role: Some(RegisterRole::Other("EXTRA")),
        id: RegisterId(0b10100),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
];

static CORTEX_M_WITH_FP_REGS_SET: &[CoreRegister] = &[
    CoreRegister {
        name: "FPSCR",
        role: Some(RegisterRole::FloatingPointStatus),
        id: RegisterId(33),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "S0",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(64),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S1",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(65),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S2",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(66),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S3",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(67),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S4",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(68),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S5",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(69),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S6",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(70),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S7",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(71),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S8",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(72),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S9",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(73),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S10",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(74),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S11",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(75),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S12",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(76),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S13",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(77),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S14",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(78),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S15",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(79),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S16",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(80),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S17",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(81),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S18",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(82),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S19",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(83),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S20",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(84),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S21",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(85),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S22",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(86),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S23",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(87),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S24",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(88),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S25",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(89),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S26",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(90),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S27",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(91),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S28",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(92),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S29",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(93),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S30",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(94),
        data_type: RegisterDataType::FloatingPoint(32),
    },
    CoreRegister {
        name: "S31",
        role: Some(RegisterRole::FloatingPoint),
        id: RegisterId(95),
        data_type: RegisterDataType::FloatingPoint(32),
    },
];
