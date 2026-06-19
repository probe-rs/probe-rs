//! Sequences for Silicon Labs EFM32 Series 1 chips (Cortex-M3/M4, ARMv7-M).
//!
//! Series 1 Geckos (e.g. EFM32PG1, EFM32JG1, EFR32xG1x) are not handled by the
//! generic [`DefaultArmSequence`](crate::architecture::arm::sequences::DefaultArmSequence).
//! Their `DEMCR.VC_CORERESET` vector catch does not reliably halt the core out of
//! reset, so a connect-under-reset attach lets the firmware run and enter a sleep
//! mode (`DHCSR.S_SLEEP=1`, `S_HALT=0`) before probe-rs can halt it, and the attach
//! times out.
//!
//! Following Silicon Labs' own approach (and the Series 2 sequence in
//! [`super::efm32xg2`]), we instead set a hardware FPB breakpoint on the reset
//! vector, which halts the core inside the reset handler before it can sleep.
//! The difference from Series 2 is the FPB comparator encoding: Series 1 is
//! ARMv7-M (FPB rev 0/1), so we reuse the rev-aware encoders from
//! [`crate::architecture::arm::core::armv7m`] rather than the ARMv8-M format.

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use probe_rs_target::{Chip, CoreType};

use crate::{
    architecture::arm::{
        ArmDebugInterface, ArmError, DapProbe, FullyQualifiedApAddress, Pins,
        core::armv7m::{Aircr, Demcr, Dhcsr, FpCtrl, FpRev1CompX, FpRev2CompX},
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, cortex_m_wait_for_reset},
    },
    core::MemoryMappedRegister,
};

/// The sequence handle for the EFM32 Series 1 family.
///
/// Uses a hardware breakpoint on the reset vector for the reset catch.
#[derive(Debug, Clone)]
pub struct EFM32xG1 {
    flash_base_addr: u64,
}

impl EFM32xG1 {
    /// Create a sequence handle for the EFM32xG1.
    pub fn create(_chip: &Chip) -> Arc<dyn ArmDebugSequence> {
        // Series 1 main flash is mapped at address 0.
        Arc::new(Self { flash_base_addr: 0 })
    }
}

impl ArmDebugSequence for EFM32xG1 {
    fn reset_catch_set(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let reset_vector = core.read_word_32(self.flash_base_addr + 0x4)?;

        // The reset vector carries the Thumb bit; the breakpoint address is the
        // half-word-aligned instruction address. An FPB instruction breakpoint can
        // only cover the code region (< 0x2000_0000), so for a blank (0xFFFFFFFF) or
        // otherwise out-of-range vector we fall back to the DEMCR vector catch and
        // rely on the force-halt in `reset_hardware_deassert`.
        let addr = reset_vector & !0b1;
        let can_breakpoint = reset_vector != 0xffff_ffff && addr < 0x2000_0000;

        if can_breakpoint {
            tracing::info!("Breakpoint on user application reset vector ({reset_vector:#010x})");

            // FPB comparator encoding differs between revisions. Pick the right one
            // based on FP_CTRL.REV, matching `armv7m::set_hw_breakpoint`.
            let ctrl = FpCtrl::from(core.read_word_32(FpCtrl::get_mmio_address())?);
            let comp: u32 = match ctrl.rev() {
                0 => FpRev1CompX::breakpoint_configuration(addr)?.into(),
                1 => FpRev2CompX::breakpoint_configuration(addr).into(),
                other => {
                    return Err(ArmError::Other(format!(
                        "FPB revision {other} is not supported for the EFM32 reset catch"
                    )));
                }
            };

            // FP_COMP0 (shared address for rev 1 and rev 2 comparator layouts).
            core.write_word_32(FpRev1CompX::get_mmio_address(), comp)?;

            // Enable the Flash Patch unit.
            let mut ctrl = FpCtrl::from(0);
            ctrl.set_key(true);
            ctrl.set_enable(true);
            core.write_word_32(FpCtrl::get_mmio_address(), ctrl.into())?;
        } else {
            tracing::info!(
                "Reset vector {reset_vector:#010x} not usable for a breakpoint, enabling vector catch"
            );
            let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
            demcr.set_vc_corereset(true);
            core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
        }

        let _ = core.read_word_32(Dhcsr::get_mmio_address())?;

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Disable FP_COMP0.
        core.write_word_32(FpRev1CompX::get_mmio_address(), 0)?;

        // Disable the Flash Patch unit (KEY must be set for the write to take effect).
        let mut ctrl = FpCtrl::from(0);
        ctrl.set_key(true);
        ctrl.set_enable(false);
        core.write_word_32(FpCtrl::get_mmio_address(), ctrl.into())?;

        let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
        demcr.set_vc_corereset(false);
        core.write_word_32(Demcr::get_mmio_address(), demcr.into())
    }

    fn reset_hardware_assert(&self, interface: &mut dyn DapProbe) -> Result<(), ArmError> {
        // Pulse RESETn (assert low, then release), ending with reset RELEASED.
        //
        // Two reasons this is a pulse rather than a hold:
        //  - Recovery: resuming a blank/locked EFM32 (e.g. at the end of a previous
        //    session) leaves the SWD-DP unresponsive — DPIDR returns NoAck — until the
        //    next reset. Pulsing here via the pin (not SWD) un-wedges the DP so the
        //    connect that follows can succeed, instead of needing a power cycle.
        //  - CSYSPWRUPACK: EFM32 does not acknowledge the *system* power-up request
        //    while RESETn is held low (CTRL/STAT stuck at CDBGPWRUPACK=1,
        //    CSYSPWRUPACK=0), which would hang the DP power-up. Releasing reset before
        //    returning lets that handshake complete.
        let mut n_reset = Pins(0);
        n_reset.set_nreset(true);
        let n_reset = n_reset.0 as u32;

        interface.swj_pins(0, n_reset, 0)?; // assert (drive low)
        thread::sleep(Duration::from_millis(20));
        interface.swj_pins(n_reset, n_reset, 0)?; // release (drive high)
        thread::sleep(Duration::from_millis(10));
        Ok(())
    }

    fn reset_hardware_deassert(
        &self,
        probe: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        // Reset was already pulsed (and released) in `reset_hardware_assert`; nothing
        // to deassert. Make sure RESETn is high, then halt the core.
        let mut n_reset = Pins(0);
        n_reset.set_nreset(true);
        let n_reset = n_reset.0 as u32;
        let _ = probe.swj_pins(n_reset, n_reset, 0)?;

        // A blank EFM32 (invalid reset vector) sits in lockup / a reset loop, and a
        // sleeping one runs into a low-energy mode; the reset vector catch does not
        // reliably stop either, so `wait_for_core_halted` would time out. Actively
        // request a halt instead — a halt request also wakes a sleeping Cortex-M and
        // catches a reset-looping core — and poll until `S_HALT`, per Silicon Labs
        // application note AN0062 section 3.1 "Halting the CPU". `reset_catch_set`
        // also armed `VC_CORERESET`, so a self-resetting core halts on its next reset.
        let mut memory = probe.memory_interface(default_ap)?;

        let mut request_halt = Dhcsr(0);
        request_halt.set_c_halt(true);
        request_halt.set_c_debugen(true);
        request_halt.enable_write();

        let start = Instant::now();
        loop {
            // Tolerate transient AP errors right after the reset pulse (the system
            // power domain may take a moment to re-acknowledge); only surface a
            // failure once the overall timeout elapses.
            let halted = (|| -> Result<bool, ArmError> {
                memory.write_word_32(Dhcsr::get_mmio_address(), request_halt.into())?;
                Ok(Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?).s_halt())
            })();

            match halted {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(e) => {
                    if start.elapsed() >= Duration::from_millis(500) {
                        return Err(e);
                    }
                }
            }

            if start.elapsed() >= Duration::from_millis(500) {
                return Err(ArmError::Timeout);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        interface.write_word_32(Aircr::get_mmio_address(), aircr.into())?;
        cortex_m_wait_for_reset(interface)?;

        let dhcsr = Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address())?);
        if dhcsr.s_lockup() {
            // Resolve lockup the same way the Series 2 sequence does, per Silicon Labs
            // application note AN0062 section 3.1 "Halting the CPU": halt, arm a
            // halt-on-reset, then trigger a local VECTRESET (valid on ARMv7-M).

            // Request halting the core for debugging.
            let mut value = Dhcsr(0);
            value.set_c_halt(true);
            value.set_c_debugen(true);
            value.enable_write();
            interface.write_word_32(Dhcsr::get_mmio_address(), value.into())?;

            // Request halt-on-reset.
            let mut demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);
            demcr.set_vc_corereset(true);
            interface.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

            // Trigger reset.
            let mut aircr = Aircr(0);
            aircr.vectkey();
            aircr.set_vectreset(true);
            aircr.set_vectclractive(true);
            interface.write_word_32(Aircr::get_mmio_address(), aircr.into())?;

            cortex_m_wait_for_reset(interface)?;
        }

        Ok(())
    }
}
