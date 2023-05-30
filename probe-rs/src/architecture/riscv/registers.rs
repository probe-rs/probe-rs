use crate::{
    core::{CoreRegister, RegisterDataType, RegisterId, RegisterRole},
    RegisterFile,
};
use once_cell::sync::Lazy;

pub const S0: CoreRegister = CoreRegister {
    name: "s0",
    role: None,
    /// This is a CSR register
    id: RegisterId(0x1008),
    data_type: RegisterDataType::UnsignedInteger,
    size_in_bits: 32,
};

pub const S1: CoreRegister = CoreRegister {
    name: "s1",
    role: None,
    /// This is a CSR register
    id: RegisterId(0x1009),
    data_type: RegisterDataType::UnsignedInteger,
    size_in_bits: 32,
};

pub(crate) static RISCV_REGISTER_FILE: Lazy<RegisterFile> =
    Lazy::new(|| RegisterFile::new(RISCV_REGISTERS_SET.iter().collect()));

static RISCV_REGISTERS_SET: &[CoreRegister] = &[
    CoreRegister {
        name: "x0",
        role: Some(RegisterRole::Other("zero")),
        id: RegisterId(0x1000),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x1",
        role: Some(RegisterRole::ReturnAddress),
        id: RegisterId(0x1001),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x2",
        role: Some(RegisterRole::StackPointer),
        id: RegisterId(0x1002),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x3",
        role: Some(RegisterRole::Other("gp")),
        id: RegisterId(0x1003),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x4",
        role: Some(RegisterRole::Other("tp")),
        id: RegisterId(0x1004),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x5",
        role: Some(RegisterRole::Other("t0")),
        id: RegisterId(0x1005),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x6",
        role: Some(RegisterRole::Other("t1")),
        id: RegisterId(0x1006),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x7",
        role: Some(RegisterRole::Other("t2")),
        id: RegisterId(0x1007),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x8",
        role: Some(RegisterRole::FramePointer),
        id: RegisterId(0x1008),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x9",
        role: Some(RegisterRole::Other("s1")),
        id: RegisterId(0x1009),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x10",
        role: Some(RegisterRole::ArgumentAndResult("a0")),
        id: RegisterId(0x100A),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x11",
        role: Some(RegisterRole::ArgumentAndResult("a1")),
        id: RegisterId(0x100B),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x12",
        role: Some(RegisterRole::Argument("a2")),
        id: RegisterId(0x100C),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x13",
        role: Some(RegisterRole::Argument("a3")),
        id: RegisterId(0x100D),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x14",
        role: Some(RegisterRole::Argument("a4")),
        id: RegisterId(0x100E),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x15",
        role: Some(RegisterRole::Argument("a5")),
        id: RegisterId(0x100F),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x16",
        role: Some(RegisterRole::Argument("a6")),
        id: RegisterId(0x1010),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x17",
        role: Some(RegisterRole::Argument("a7")),
        id: RegisterId(0x1011),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x18",
        role: Some(RegisterRole::Other("s2")),
        id: RegisterId(0x1012),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x19",
        role: Some(RegisterRole::Other("s3")),
        id: RegisterId(0x1013),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x20",
        role: Some(RegisterRole::Other("s4")),
        id: RegisterId(0x1014),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x21",
        role: Some(RegisterRole::Other("s5")),
        id: RegisterId(0x1015),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x22",
        role: Some(RegisterRole::Other("s6")),
        id: RegisterId(0x1016),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x23",
        role: Some(RegisterRole::Other("s7")),
        id: RegisterId(0x1017),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x24",
        role: Some(RegisterRole::Other("s8")),
        id: RegisterId(0x1018),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x25",
        role: Some(RegisterRole::Other("s9")),
        id: RegisterId(0x1019),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x26",
        role: Some(RegisterRole::Other("s10")),
        id: RegisterId(0x101A),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x27",
        role: Some(RegisterRole::Other("s11")),
        id: RegisterId(0x101B),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x28",
        role: Some(RegisterRole::Other("t3")),
        id: RegisterId(0x101C),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x29",
        role: Some(RegisterRole::Other("t4")),
        id: RegisterId(0x101D),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x30",
        role: Some(RegisterRole::Other("t5")),
        id: RegisterId(0x101E),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "x31",
        role: Some(RegisterRole::Other("t6")),
        id: RegisterId(0x101F),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    CoreRegister {
        name: "pc",
        role: Some(RegisterRole::ProgramCounter),
        /// This is a CSR register
        id: RegisterId(0x7b1),
        data_type: RegisterDataType::UnsignedInteger,
        size_in_bits: 32,
    },
    // TODO: Add FPU registers
];
