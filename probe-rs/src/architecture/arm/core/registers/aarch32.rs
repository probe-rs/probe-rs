use super::cortex_m::ARM32_COMMON_REGS_SET;
use crate::{
    core::{RegisterDataType, RegisterRole},
    CoreRegister, CoreRegisters, RegisterId,
};
use once_cell::sync::Lazy;

pub(crate) static AARCH32_CORE_REGSISTERS: Lazy<CoreRegisters> = Lazy::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(AARCH32_COMMON_REGS_SET)
            .collect::<Vec<_>>(),
    )
});

pub(crate) static AARCH32_WITH_FP_16_CORE_REGSISTERS: Lazy<CoreRegisters> = Lazy::new(|| {
    CoreRegisters::new(
        ARM32_COMMON_REGS_SET
            .iter()
            .chain(AARCH32_COMMON_REGS_SET)
            .chain(AARCH32_FP_16_REGS_SET)
            .collect(),
    )
});

pub(crate) static AARCH32_WITH_FP_32_CORE_REGSISTERS: Lazy<CoreRegisters> = Lazy::new(|| {
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
    name: "CPSR",
    roles: &[RegisterRole::ProcessorStatus],
    id: RegisterId(0b1_0000),
    data_type: RegisterDataType::UnsignedInteger(32),
}];

static AARCH32_FP_16_REGS_SET: &[CoreRegister] = &[
    CoreRegister {
        name: "FPSCR",
        roles: &[RegisterRole::FloatingPointStatus],
        id: RegisterId(49),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "D0",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(17),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D1",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(18),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D2",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(19),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D3",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(20),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D4",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(21),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D5",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(22),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D6",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(23),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D7",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(24),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D8",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(25),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D9",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(26),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D10",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(27),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D11",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(28),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D12",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(29),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D13",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(30),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D14",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(31),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D15",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(32),
        data_type: RegisterDataType::FloatingPoint(64),
    },
];

static AARCH32_FP_32_REGS_SET: &[CoreRegister] = &[
    CoreRegister {
        name: "D16",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(33),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D17",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(34),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D18",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(35),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D19",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(36),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D20",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(37),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D21",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(38),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D22",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(39),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D23",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(40),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D24",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(41),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D25",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(42),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D26",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(43),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D27",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(44),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D28",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(45),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D29",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(46),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D30",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(47),
        data_type: RegisterDataType::FloatingPoint(64),
    },
    CoreRegister {
        name: "D31",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(48),
        data_type: RegisterDataType::FloatingPoint(64),
    },
];
