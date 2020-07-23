use crate::{
    core::{CoreRegister, CoreRegisterAddress, RegisterDescription, RegisterFile, RegisterKind},
    CoreStatus, Error, HaltReason, MemoryInterface,
};

use bitfield::bitfield;

pub mod m0;
pub mod m33;
pub mod m4;

/// Enable debugging on an ARM core. This is based on the
/// `DebugCoreStart` function from the [ARM SVD Debug Description].
///
/// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#debugCoreStart
pub(crate) fn debug_core_start(core: &mut impl MemoryInterface) -> Result<(), Error> {
    use crate::architecture::arm::core::m4::Dhcsr;

    let mut dhcsr = Dhcsr(0);
    dhcsr.set_c_debugen(true);
    dhcsr.enable_write();

    core.write_word_32(Dhcsr::ADDRESS, dhcsr.into())?;

    Ok(())
}

/// Setup the core to stop after reset. After this, the core will halt when it comes
/// out of reset. This is based on the `ResetCatchSet` function from
/// the [ARM SVD Debug Description].
///
/// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#resetCatchSet
pub(crate) fn reset_catch_set(core: &mut impl MemoryInterface) -> Result<(), Error> {
    use crate::architecture::arm::core::m4::{Demcr, Dhcsr};

    // Request halt after reset
    let mut demcr = Demcr(core.read_word_32(Demcr::ADDRESS)?);
    demcr.set_vc_corereset(true);

    core.write_word_32(Demcr::ADDRESS, demcr.into())?;

    // Clear the status bits by reading from DHCSR
    let _ = core.read_word_32(Dhcsr::ADDRESS)?;

    Ok(())
}

/// Undo the settings of the `reset_catch_set` function.
/// This is based on the `ResetCatchSet` function from
/// the [ARM SVD Debug Description].
///
/// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#resetCatchClear
pub(crate) fn reset_catch_clear(core: &mut impl MemoryInterface) -> Result<(), Error> {
    use crate::architecture::arm::core::m4::Demcr;

    // Clear reset catch bit
    let mut demcr = Demcr(core.read_word_32(Demcr::ADDRESS)?);
    demcr.set_vc_corereset(false);

    core.write_word_32(Demcr::ADDRESS, demcr.into())?;
    Ok(())
}

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

pub(crate) mod register {
    use crate::{
        core::{RegisterDescription, RegisterKind},
        CoreRegisterAddress,
    };

    pub const PC: RegisterDescription = RegisterDescription {
        name: "PC",
        kind: RegisterKind::PC,
        address: CoreRegisterAddress(15),
    };

    pub const XPSR: RegisterDescription = RegisterDescription {
        name: "XPSR",
        kind: RegisterKind::General,
        address: CoreRegisterAddress(0b1_0000),
    };

    pub const SP: RegisterDescription = RegisterDescription {
        name: "SP",
        kind: RegisterKind::General,
        address: CoreRegisterAddress(13),
    };

    pub const LR: RegisterDescription = RegisterDescription {
        name: "LR",
        kind: RegisterKind::General,
        address: CoreRegisterAddress(14),
    };
}

static ARM_REGISTER_FILE: RegisterFile = RegisterFile {
    platform_registers: &[
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

    program_counter: &register::PC,
    stack_pointer: &register::SP,
    return_address: &register::LR,

    argument_registers: &[
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

    result_registers: &[
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
        if self.0.count_ones() != 1 {
            // We cannot identify why the chip halted,
            // it could be for multiple reasons.
            HaltReason::Unknown
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
pub(crate) struct CortexState {
    initialized: bool,

    hw_breakpoints_enabled: bool,

    current_state: CoreStatus,
}

impl CortexState {
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
