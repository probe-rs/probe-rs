//! Sequences for XMC1000 (XMC1100/1200/1300/1400).
//!
//! The XMC1xxx ROM startup software (SSW) is the first code executed after any
//! reset. It disables debug access to the system address range while it runs,
//! and only re-enables it at the end if the Boot Mode Index selects a
//! debug-capable boot mode. `AIRCR.SYSRESETREQ` therefore re-runs the SSW with
//! the debug port inaccessible, so we avoid a system reset for the
//! reset-and-halt used before flashing: the running application has already
//! completed the SSW, so the core can simply be halted.
//!
//! An explicit "reset" after flashing is implemented as a warm start: the
//! stack pointer and program counter are loaded from the application vector
//! table and the core is resumed, without touching `SYSRESETREQ`.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use probe_rs_target::CoreType;

use crate::{
    MemoryMappedRegister, RegisterId,
    architecture::arm::{
        ArmError,
        armv6m::{Demcr, Dhcsr},
        core::cortex_m,
        memory::ArmMemoryInterface,
        sequences::ArmDebugSequence,
    },
};

/// Start of the user application vector table on XMC1xxx (see the linker
/// script: FLASH @ 0x1000_1000). Word 0 is the initial SP, word 1 the reset
/// vector.
const APP_VECTOR_TABLE: u64 = 0x1000_1000;

/// `DCRSR` register selectors
const REGSEL_SP: u16 = 13;
const REGSEL_PC: u16 = 15;
const REGSEL_XPSR: u16 = 16;

/// An Infineon XMC1xxx MCU.
#[derive(Debug, Default)]
pub struct XMC1000 {
    /// Set by [`ArmDebugSequence::reset_catch_set`], read by
    /// [`ArmDebugSequence::reset_system`] to distinguish reset-and-halt from a plain reset.
    halt_after_reset: AtomicBool,
}

impl XMC1000 {
    /// Create the sequencer for an Infineon XMC1000.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self::default())
    }
}

/// Halt or resume the core via `DHCSR.C_HALT`.
fn set_halted(interface: &mut dyn ArmMemoryInterface, halted: bool) -> Result<(), ArmError> {
    let mut dhcsr = Dhcsr(0);
    dhcsr.set_c_debugen(true);
    dhcsr.set_c_halt(halted);
    dhcsr.enable_write();

    interface.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
    interface.flush()?;

    Ok(())
}

/// Position the (halted) core at the application entry point without a system
/// reset: load SP and PC from the application vector table.
fn position_at_app_entry(interface: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    let initial_sp = interface.read_word_32(APP_VECTOR_TABLE)?;
    let reset_vector = interface.read_word_32(APP_VECTOR_TABLE + 4)?;

    tracing::debug!(
        "XMC1000: positioning core at application entry, SP={:#010x}, PC={:#010x}",
        initial_sp,
        reset_vector
    );

    cortex_m::write_core_reg(interface, RegisterId(REGSEL_SP), initial_sp)?;
    cortex_m::write_core_reg(interface, RegisterId(REGSEL_PC), reset_vector)?;
    // Set the T-bit so the core stays in Thumb state after resuming.
    cortex_m::write_core_reg(interface, RegisterId(REGSEL_XPSR), 0x0100_0000)?;

    Ok(())
}

impl ArmDebugSequence for XMC1000 {
    fn reset_catch_set(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::trace!("performing XMC1000 ResetCatchSet");

        // `reset_system` halts the core without a system reset, so no reset
        // vector catch is armed. Clear any stale catch and record the halt request.
        let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
        demcr.set_vc_corereset(false);
        core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

        self.halt_after_reset.store(true, Ordering::Relaxed);

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::trace!("performing XMC1000 ResetCatchClear");

        // No reset vector catch is used; make sure none is left armed.
        let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
        demcr.set_vc_corereset(false);
        core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

        self.halt_after_reset.store(false, Ordering::Relaxed);

        Ok(())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::trace!("performing XMC1000 ResetSystem");

        let halt_after_reset = self.halt_after_reset.swap(false, Ordering::Relaxed);

        // DCRSR/DCRDR transfers require a halted core.
        set_halted(interface, true)?;
        position_at_app_entry(interface)?;

        if halt_after_reset {
            // Leave the core halted at the application entry; a later resume
            // starts the freshly flashed image.
            tracing::debug!("XMC1000: halted core at application entry without system reset");
        } else {
            // Plain reset: start the application from its vector table.
            tracing::debug!("XMC1000: warm-starting application without system reset");
            set_halted(interface, false)?;
        }

        Ok(())
    }
}
