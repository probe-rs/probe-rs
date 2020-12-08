use std::{
    thread,
    time::{Duration, Instant},
};

use crate::{
    core::{CoreRegister, CoreRegisterAddress, RegisterDescription, RegisterFile, RegisterKind},
    CoreStatus, DebugProbeError, Error, HaltReason, MemoryInterface,
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

    let current_dhcsr = Dhcsr(core.read_word_32(Dhcsr::ADDRESS)?);

    // Note: Manual addition for debugging, not part of the original DebugCoreStart function
    if current_dhcsr.c_debugen() {
        log::debug!("Core is already in debug mode, no need to enable it again");
        return Ok(());
    }
    // -- End addition

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

/*

LPC55S69 specific sequence

pub(crate) fn reset_catch_set(core: &mut impl MemoryInterface) -> Result<(), Error> {
    use crate::architecture::arm::core::m4::{Demcr, Dhcsr};

    dbg!("reset_catch_set");

    let mut reset_vector = 0xffff_ffff;
    let mut demcr = Demcr(core.read_word_32(Demcr::ADDRESS)?);

    demcr.set_vc_corereset(false);

    core.write_word_32(Demcr::ADDRESS, demcr.into())?;

    // Write some stuff
    core.write_word_32(0x40034010, 0x00000000)?; // Program Flash Word Start Address to 0x0 to read reset vector (STARTA)
    core.write_word_32(0x40034014, 0x00000000)?; // Program Flash Word Stop Address to 0x0 to read reset vector (STOPA)
    core.write_word_32(0x40034080, 0x00000000)?; // DATAW0: Prepare for read
    core.write_word_32(0x40034084, 0x00000000)?; // DATAW1: Prepare for read
    core.write_word_32(0x40034088, 0x00000000)?; // DATAW2: Prepare for read
    core.write_word_32(0x4003408C, 0x00000000)?; // DATAW3: Prepare for read
    core.write_word_32(0x40034090, 0x00000000)?; // DATAW4: Prepare for read
    core.write_word_32(0x40034094, 0x00000000)?; // DATAW5: Prepare for read
    core.write_word_32(0x40034098, 0x00000000)?; // DATAW6: Prepare for read
    core.write_word_32(0x4003409C, 0x00000000)?; // DATAW7: Prepare for read

    core.write_word_32(0x40034FE8, 0x0000000F)?; // Clear FLASH Controller Status (INT_CLR_STATUS)
    core.write_word_32(0x40034000, 0x00000003)?; // Read single Flash Word (CMD_READ_SINGLE_WORD)

    let start = Instant::now();

    let mut timeout = true;

    while start.elapsed() < Duration::from_micros(10_0000) {
        let value = core.read_word_32(0x40034FE0)?;

        if (value & 0x4) == 0x4 {
            timeout = false;
            break;
        }
    }

    if timeout {
        log::warn!("Failed: Wait for flash word read to finish");
        return Err(Error::Probe(DebugProbeError::Timeout));
    }

    if (core.read_word_32(0x4003_4fe0)? & 0xB) == 0 {
        log::info!("No Error reading Flash Word with Reset Vector");

        reset_vector = core.read_word_32(0x0000_0004)?;
    }

    if reset_vector != 0xffff_ffff {
        log::info!("Breakpoint on user application reset vector");

        core.write_word_32(0xE000_2008, reset_vector | 1)?;
        core.write_word_32(0xE000_2000, 3)?;
    }

    if reset_vector == 0xffff_ffff {
        log::info!("Enable reset vector catch");

        let mut demcr = Demcr(core.read_word_32(Demcr::ADDRESS)?);

        demcr.set_vc_corereset(true);

        core.write_word_32(Demcr::ADDRESS, demcr.into())?;
    }

    let _ = core.read_word_32(Dhcsr::ADDRESS)?;

    Ok(())
}
*/

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

/*

LPC55S69 specific sequence

pub(crate) fn reset_catch_clear(core: &mut impl MemoryInterface) -> Result<(), Error> {
    use crate::architecture::arm::core::m4::Demcr;

    core.write_word_32(0xE000_2008, 0x0)?;
    core.write_word_32(0xE000_2000, 0x2)?;

    let mut demcr = Demcr(core.read_word_32(Demcr::ADDRESS)?);

    demcr.set_vc_corereset(false);

    core.write_word_32(Demcr::ADDRESS, demcr.into())
}
*/

pub(crate) fn reset_system(core: &mut impl MemoryInterface) -> Result<(), Error> {
    use crate::architecture::arm::core::m4::{Aircr, Dhcsr};

    let mut aircr = Aircr(0);
    aircr.vectkey();
    aircr.set_sysresetreq(true);

    core.write_word_32(Aircr::ADDRESS, aircr.into())?;

    let start = Instant::now();

    while start.elapsed() < Duration::from_micros(50_0000) {
        let dhcsr = Dhcsr(core.read_word_32(Dhcsr::ADDRESS)?);

        // Wait until the S_RESET_ST bit is cleared on a read
        if !dhcsr.s_reset_st() {
            return Ok(());
        }
    }

    Err(Error::Probe(DebugProbeError::Timeout))
}

/*

// TODO: Remove this, hacked version for lpc555s69
pub(crate) fn reset_system(core: &mut impl MemoryInterface) -> Result<(), Error> {
    use crate::architecture::arm::core::m4::{Aircr, Dhcsr};

    let mut aircr = Aircr(0);
    aircr.vectkey();
    aircr.set_sysresetreq(true);

    let result = core.write_word_32(Aircr::ADDRESS, aircr.into());

    if let Err(e) = result {
        log::debug!("Error requesting reset: {:?}", e);
    }

    thread::sleep(Duration::from_millis(10));

    wait_for_stop_after_reset(core)
}

*/

fn wait_for_stop_after_reset(core: &mut impl MemoryInterface) -> Result<(), Error> {
    use crate::architecture::arm::core::m4::Dhcsr;

    thread::sleep(Duration::from_millis(10));

    let mut timeout = true;

    let start = Instant::now();

    while start.elapsed() < Duration::from_micros(50_0000) {
        let dhcsr = Dhcsr(core.read_word_32(Dhcsr::ADDRESS)?);

        if !dhcsr.s_reset_st() {
            timeout = false;
            break;
        }
    }

    if timeout {
        return Err(Error::Probe(DebugProbeError::Timeout));
    }

    let dhcsr = Dhcsr(core.read_word_32(Dhcsr::ADDRESS)?);

    if !dhcsr.s_halt() {
        let mut dhcsr = Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);

        core.write_word_32(Dhcsr::ADDRESS, dhcsr.into())?;
    }

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
        RegisterDescription {
            name: "XPSR",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0b10000),
        },
        RegisterDescription {
            name: "state (todo)",
            kind: RegisterKind::General,
            address: CoreRegisterAddress(0b10100),
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
pub struct CortexState {
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
