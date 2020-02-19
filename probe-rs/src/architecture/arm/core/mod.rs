use crate::core::CoreRegisterAddress;
use crate::core::RegisterDescription;
use crate::core::RegisterFile;
use crate::core::RegisterKind;

pub mod m0;
pub mod m33;
pub mod m4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexDump {
    pub regs: [u32; 16],
    stack_addr: u32,
    stack: Vec<u8>,
}

impl CortexDump {
    pub fn new(stack_addr: u32, stack: Vec<u8>) -> CortexDump {
        CortexDump {
            regs: [0u32; 16],
            stack_addr,
            stack,
        }
    }
}

fn arm_register_file() -> RegisterFile {
    RegisterFile {
        platform_registers: vec![
            RegisterDescription {
                name: "R0",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(0),
            },
            RegisterDescription {
                name: "R1",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(1),
            },
            RegisterDescription {
                name: "R2",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(2),
            },
            RegisterDescription {
                name: "R3",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(3),
            },
            RegisterDescription {
                name: "R4",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(4),
            },
            RegisterDescription {
                name: "R5",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(5),
            },
            RegisterDescription {
                name: "R6",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(6),
            },
            RegisterDescription {
                name: "R7",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(7),
            },
            RegisterDescription {
                name: "R8",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(8),
            },
            RegisterDescription {
                name: "R9",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(9),
            },
            RegisterDescription {
                name: "R10",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(10),
            },
            RegisterDescription {
                name: "R11",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(11),
            },
            RegisterDescription {
                name: "R12",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(12),
            },
            RegisterDescription {
                name: "R13",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(13),
            },
            RegisterDescription {
                name: "R14",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(14),
            },
            RegisterDescription {
                name: "R15",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(15),
            },
        ],

        program_counter: RegisterDescription {
            name: "PC",
            kind: RegisterKind::PC,
            address: CoreRegisterAddress(15),
        },

        stack_pointer: RegisterDescription {
            name: "SP",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(13),
        },

        return_address: RegisterDescription {
            name: "LR",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(14),
        },

        argument_registers: vec![
            RegisterDescription {
                name: "a1",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(0),
            },
            RegisterDescription {
                name: "a2",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(1),
            },
            RegisterDescription {
                name: "a3",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(2),
            },
            RegisterDescription {
                name: "a4",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(3),
            },
        ],

        result_registers: vec![
            RegisterDescription {
                name: "a1",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(0),
            },
            RegisterDescription {
                name: "a2",
                kind: RegisterKind::General,
                address: CoreRegisterAddress(1),
            },
        ],
    }
}
