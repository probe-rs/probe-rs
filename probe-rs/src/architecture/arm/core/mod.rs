use crate::{
    core::{CoreRegister, CoreRegisterAddress, RegisterDescription, RegisterFile, RegisterKind},
    CoreStatus, HaltReason,
};

use bitfield::bitfield;

pub mod armv6m;
pub mod armv7a;
pub mod armv7m;
pub mod armv8a;
pub mod armv8m;

pub(crate) mod armv7a_debug_regs;
pub(crate) mod armv8a_debug_regs;
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
        core::{RegisterDescription, RegisterKind},
        CoreRegisterAddress,
    };

    pub const PC: RegisterDescription = RegisterDescription {
        name: "PC",
        _kind: RegisterKind::PC,
        address: CoreRegisterAddress(15),
    };

    pub const XPSR: RegisterDescription = RegisterDescription {
        name: "XPSR",
        _kind: RegisterKind::General,
        address: CoreRegisterAddress(0b1_0000),
    };

    pub const SP: RegisterDescription = RegisterDescription {
        name: "SP",
        _kind: RegisterKind::General,
        address: CoreRegisterAddress(13),
    };

    pub const LR: RegisterDescription = RegisterDescription {
        name: "LR",
        _kind: RegisterKind::General,
        address: CoreRegisterAddress(14),
    };

    pub const MSP: RegisterDescription = RegisterDescription {
        name: "MSP",
        _kind: RegisterKind::General,
        address: CoreRegisterAddress(0b10001),
    };

    pub const PSP: RegisterDescription = RegisterDescription {
        name: "PSP",
        _kind: RegisterKind::General,
        address: CoreRegisterAddress(0b10010),
    };

    // CONTROL bits [31:24], FAULTMASK bits [23:16],
    // BASEPRI bits [15:8], and PRIMASK bits [7:0]
    pub const EXTRA: RegisterDescription = RegisterDescription {
        name: "EXTRA",
        _kind: RegisterKind::General,
        address: CoreRegisterAddress(0b10100),
    };

    // TODO: Floating point support
    pub const FP: RegisterDescription = RegisterDescription {
        name: "FP",
        _kind: RegisterKind::General,
        address: CoreRegisterAddress(7),
    };
}

static ARM_REGISTER_FILE: RegisterFile = RegisterFile {
    platform_registers: &[
        RegisterDescription {
            name: "R0",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0),
        },
        RegisterDescription {
            name: "R1",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(1),
        },
        RegisterDescription {
            name: "R2",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(2),
        },
        RegisterDescription {
            name: "R3",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(3),
        },
        RegisterDescription {
            name: "R4",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(4),
        },
        RegisterDescription {
            name: "R5",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(5),
        },
        RegisterDescription {
            name: "R6",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(6),
        },
        RegisterDescription {
            name: "R7",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(7),
        },
        RegisterDescription {
            name: "R8",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(8),
        },
        RegisterDescription {
            name: "R9",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(9),
        },
        RegisterDescription {
            name: "R10",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(10),
        },
        RegisterDescription {
            name: "R11",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(11),
        },
        RegisterDescription {
            name: "R12",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(12),
        },
        RegisterDescription {
            name: "R13",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(13),
        },
        RegisterDescription {
            name: "R14",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(14),
        },
        RegisterDescription {
            name: "R15",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(15),
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
            address: CoreRegisterAddress(0),
        },
        RegisterDescription {
            name: "a2",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(1),
        },
        RegisterDescription {
            name: "a3",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(2),
        },
        RegisterDescription {
            name: "a4",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(3),
        },
    ],

    result_registers: &[
        RegisterDescription {
            name: "a1",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0),
        },
        RegisterDescription {
            name: "a2",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(1),
        },
    ],

    msp: Some(&register::MSP),
    psp: Some(&register::PSP),
    extra: Some(&register::EXTRA),
    // TODO: Floating point support
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

    fn halt_reason(&self) -> HaltReason {
        if self.0 == 0 {
            // No bit is set
            HaltReason::Unknown
        } else if self.0.count_ones() > 1 {
            log::debug!("DFSR: {:?}", self);

            // We cannot identify why the chip halted,
            // it could be for multiple reasons.

            // For debuggers, it's important to know if
            // the core halted because of a breakpoint.
            // Because of this, we still return breakpoint
            // even if other reasons are possible as well.
            if self.bkpt() {
                HaltReason::Breakpoint
            } else {
                HaltReason::Multiple
            }
        } else if self.bkpt() {
            HaltReason::Breakpoint
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

impl CoreRegister for Dfsr {
    const ADDRESS: u32 = 0xE000_ED30;
    const NAME: &'static str = "DFSR";
}

#[derive(Debug)]
pub struct CortexMState {
    initialized: bool,

    hw_breakpoints_enabled: bool,

    current_state: CoreStatus,
}

impl CortexMState {
    pub(crate) fn new() -> Self {
        Self {
            initialized: false,
            hw_breakpoints_enabled: false,
            current_state: CoreStatus::Unknown,
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

    register_cache: Vec<Option<(u32, bool)>>,
}

impl CortexAState {
    pub(crate) fn new() -> Self {
        Self {
            initialized: false,
            current_state: CoreStatus::Unknown,
            is_64_bit: false,
            register_cache: vec![],
        }
    }

    fn initialize(&mut self) {
        self.initialized = true;
    }

    fn initialized(&self) -> bool {
        self.initialized
    }
}
