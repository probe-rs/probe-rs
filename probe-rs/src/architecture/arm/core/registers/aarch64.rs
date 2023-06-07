use crate::{
    core::{RegisterDataType, RegisterRole},
    CoreRegister, CoreRegisters, RegisterId,
};
use once_cell::sync::Lazy;

pub(crate) const PC: CoreRegister = CoreRegister {
    name: "PC",
    roles: &[RegisterRole::ProgramCounter],
    id: RegisterId(32),
    data_type: RegisterDataType::UnsignedInteger(64),
};

pub(crate) const FP: CoreRegister = CoreRegister {
    name: "X29",
    roles: &[RegisterRole::FramePointer],
    id: RegisterId(29),
    data_type: RegisterDataType::UnsignedInteger(64),
};

pub(crate) const SP: CoreRegister = CoreRegister {
    name: "SP",
    roles: &[RegisterRole::StackPointer],
    id: RegisterId(31),
    data_type: RegisterDataType::UnsignedInteger(64),
};

pub(crate) const RA: CoreRegister = CoreRegister {
    name: "X30",
    roles: &[RegisterRole::ReturnAddress],
    id: RegisterId(30),
    data_type: RegisterDataType::UnsignedInteger(64),
};

pub(crate) static AARCH64_CORE_REGSISTERS: Lazy<CoreRegisters> =
    Lazy::new(|| CoreRegisters::new(AARCH64_CORE_REGSISTERS_SET.iter().collect()));

pub static AARCH64_CORE_REGSISTERS_SET: &[CoreRegister] = &[
    CoreRegister {
        name: "X0",
        roles: &[RegisterRole::Argument("a0"), RegisterRole::Return("r0")],
        id: RegisterId(0),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X1",
        roles: &[RegisterRole::Argument("a1"), RegisterRole::Return("r1")],
        id: RegisterId(1),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X2",
        roles: &[RegisterRole::Argument("a2")],
        id: RegisterId(2),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X3",
        roles: &[RegisterRole::Argument("a3")],
        id: RegisterId(3),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X4",
        roles: &[RegisterRole::Argument("a4")],
        id: RegisterId(4),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X5",
        roles: &[RegisterRole::Argument("a5")],
        id: RegisterId(5),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X6",
        roles: &[RegisterRole::Argument("a6")],
        id: RegisterId(6),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X7",
        roles: &[RegisterRole::Argument("a7")],
        id: RegisterId(7),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X8",
        // Indirect result location register.
        roles: &[RegisterRole::Other("XR")],
        id: RegisterId(8),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X9",
        roles: &[],
        id: RegisterId(9),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X10",
        roles: &[],
        id: RegisterId(10),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X11",
        roles: &[],
        id: RegisterId(11),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X12",
        roles: &[],
        id: RegisterId(12),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X13",
        roles: &[],
        id: RegisterId(13),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X14",
        roles: &[],
        id: RegisterId(14),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X15",
        roles: &[],
        id: RegisterId(15),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X16",
        roles: &[],
        id: RegisterId(16),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X17",
        roles: &[],
        id: RegisterId(17),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X18",
        roles: &[],
        id: RegisterId(18),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X19",
        roles: &[],
        id: RegisterId(19),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X20",
        roles: &[],
        id: RegisterId(20),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X21",
        roles: &[],
        id: RegisterId(21),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X22",
        roles: &[],
        id: RegisterId(22),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X23",
        roles: &[],
        id: RegisterId(23),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X24",
        roles: &[],
        id: RegisterId(24),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X25",
        roles: &[],
        id: RegisterId(25),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X26",
        roles: &[],
        id: RegisterId(26),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X27",
        roles: &[],
        id: RegisterId(27),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X28",
        roles: &[],
        id: RegisterId(28),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    FP,
    RA,
    SP,
    PC,
    CoreRegister {
        name: "PSTATE",
        roles: &[RegisterRole::ProcessorStatus],
        id: RegisterId(33),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "v0",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(34),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v1",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(35),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v2",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(36),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v3",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(37),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v4",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(38),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v5",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(39),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v6",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(40),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v7",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(41),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v8",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(42),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v9",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(43),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v10",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(44),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v11",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(45),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v12",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(46),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v13",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(47),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v14",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(48),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v15",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(49),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v16",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(50),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v17",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(51),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v18",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(52),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v19",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(53),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v20",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(54),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v21",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(55),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v22",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(56),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v23",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(57),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v24",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(58),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v25",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(59),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v26",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(60),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v27",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(61),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v28",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(62),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v29",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(63),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v30",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(64),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v31",
        roles: &[RegisterRole::FloatingPoint],
        id: RegisterId(65),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "FPSR",
        roles: &[RegisterRole::FloatingPointStatus],
        id: RegisterId(66),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "FPCR",
        roles: &[RegisterRole::Other("Floating Point Control")],
        id: RegisterId(67),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
];
