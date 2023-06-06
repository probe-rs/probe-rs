use crate::{
    core::{RegisterDataType, RegisterRole},
    CoreRegister, CoreRegisters, RegisterId,
};
use once_cell::sync::Lazy;

pub(crate) const PC: CoreRegister = CoreRegister {
    name: "PC",
    roles: Some(&[RegisterRole::ProgramCounter]),
    id: RegisterId(32),
    data_type: RegisterDataType::UnsignedInteger(64),
};

pub(crate) const FP: CoreRegister = CoreRegister {
    name: "X29",
    roles: Some(&[RegisterRole::FramePointer]),
    id: RegisterId(29),
    data_type: RegisterDataType::UnsignedInteger(64),
};

pub(crate) const SP: CoreRegister = CoreRegister {
    name: "SP",
    roles: Some(&[RegisterRole::StackPointer]),
    id: RegisterId(31),
    data_type: RegisterDataType::UnsignedInteger(64),
};

pub(crate) const RA: CoreRegister = CoreRegister {
    name: "X30",
    roles: Some(&[RegisterRole::ReturnAddress]),
    id: RegisterId(30),
    data_type: RegisterDataType::UnsignedInteger(64),
};

pub(crate) static AARCH64_CORE_REGSISTERS: Lazy<CoreRegisters> =
    Lazy::new(|| CoreRegisters::new(AARCH64_CORE_REGSISTERS_SET.iter().collect()));

pub static AARCH64_CORE_REGSISTERS_SET: &[CoreRegister] = &[
    CoreRegister {
        name: "X0",
        roles: Some(&[RegisterRole::Argument("a0"), RegisterRole::Return("r0")]),
        id: RegisterId(0),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X1",
        roles: Some(&[RegisterRole::Argument("a1"), RegisterRole::Return("r1")]),
        id: RegisterId(1),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X2",
        roles: Some(&[RegisterRole::Argument("a2")]),
        id: RegisterId(2),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X3",
        roles: Some(&[RegisterRole::Argument("a3")]),
        id: RegisterId(3),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X4",
        roles: Some(&[RegisterRole::Argument("a4")]),
        id: RegisterId(4),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X5",
        roles: Some(&[RegisterRole::Argument("a5")]),
        id: RegisterId(5),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X6",
        roles: Some(&[RegisterRole::Argument("a6")]),
        id: RegisterId(6),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X7",
        roles: Some(&[RegisterRole::Argument("a7")]),
        id: RegisterId(7),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X8",
        // Indirect result location register.
        roles: Some(&[RegisterRole::Other("XR")]),
        id: RegisterId(8),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X9",
        roles: None,
        id: RegisterId(9),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X10",
        roles: None,
        id: RegisterId(10),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X11",
        roles: None,
        id: RegisterId(11),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X12",
        roles: None,
        id: RegisterId(12),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X13",
        roles: None,
        id: RegisterId(13),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X14",
        roles: None,
        id: RegisterId(14),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X15",
        roles: None,
        id: RegisterId(15),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X16",
        roles: None,
        id: RegisterId(16),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X17",
        roles: None,
        id: RegisterId(17),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X18",
        roles: None,
        id: RegisterId(18),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X19",
        roles: None,
        id: RegisterId(19),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X20",
        roles: None,
        id: RegisterId(20),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X21",
        roles: None,
        id: RegisterId(21),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X22",
        roles: None,
        id: RegisterId(22),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X23",
        roles: None,
        id: RegisterId(23),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X24",
        roles: None,
        id: RegisterId(24),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X25",
        roles: None,
        id: RegisterId(25),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X26",
        roles: None,
        id: RegisterId(26),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X27",
        roles: None,
        id: RegisterId(27),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    CoreRegister {
        name: "X28",
        roles: None,
        id: RegisterId(28),
        data_type: RegisterDataType::UnsignedInteger(64),
    },
    FP,
    RA,
    SP,
    PC,
    CoreRegister {
        name: "PSTATE",
        roles: Some(&[RegisterRole::ProcessorStatus]),
        id: RegisterId(33),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "v0",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(34),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v1",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(35),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v2",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(36),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v3",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(37),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v4",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(38),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v5",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(39),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v6",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(40),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v7",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(41),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v8",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(42),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v9",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(43),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v10",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(44),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v11",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(45),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v12",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(46),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v13",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(47),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v14",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(48),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v15",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(49),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v16",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(50),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v17",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(51),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v18",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(52),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v19",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(53),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v20",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(54),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v21",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(55),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v22",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(56),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v23",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(57),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v24",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(58),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v25",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(59),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v26",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(60),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v27",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(61),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v28",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(62),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v29",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(63),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v30",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(64),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "v31",
        roles: Some(&[RegisterRole::FloatingPoint]),
        id: RegisterId(65),
        data_type: RegisterDataType::FloatingPoint(128),
    },
    CoreRegister {
        name: "FPSR",
        roles: Some(&[RegisterRole::FloatingPointStatus]),
        id: RegisterId(66),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
    CoreRegister {
        name: "FPCR",
        roles: Some(&[RegisterRole::Other("Floating Point Control")]),
        id: RegisterId(67),
        data_type: RegisterDataType::UnsignedInteger(32),
    },
];
