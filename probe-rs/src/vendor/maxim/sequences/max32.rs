//! Debug sequences for Maxim Integrated MAX32xxx microcontrollers (Cortex-M4, ARMv7-M).
//!
//! Matches OpenOCD's approach: set an FPB breakpoint at the chip's ROM pass-through
//! address before issuing SYSRESETREQ, then manually halt the core if the breakpoint
//! doesn't survive the reset.
//!
//! nSRST (hardware pin reset) is available via `--connect-under-reset`.

use std::{sync::Arc, thread, time::Duration};

use probe_rs_target::CoreType;

use crate::{
    architecture::arm::{
        ArmError, DapProbe, Pins,
        core::armv7m::{Dhcsr, FpCtrl, FpRev1CompX, FpRev2CompX},
        memory::ArmMemoryInterface,
        sequences::ArmDebugSequence,
    },
    core::MemoryMappedRegister,
};

/// Debug sequence for Maxim MAX32xxx chips.
#[derive(Debug, Clone)]
pub struct Max32 {
    rom_breakpoint: u32,
}

impl Max32 {
    /// Creates a new [`Max32`] debug sequence.
    ///
    /// `rom_breakpoint` is the address in ROM where the bootloader passes
    /// through on reset (OpenOCD sets a hardware breakpoint here).
    /// Common values: `0x0000FFF4` (MAX32690), `0x00002174` (MAX32670/75).
    pub fn create(rom_breakpoint: u32) -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self { rom_breakpoint })
    }
}

impl ArmDebugSequence for Max32 {
    fn reset_catch_set(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Halt the core before reset (matching OpenOCD's `reset-assert-pre`).
        let mut dhcsr = Dhcsr(core.read_word_32(Dhcsr::get_mmio_address())?);
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);
        dhcsr.enable_write();
        core.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

        // Set an FPB breakpoint at the ROM address (matching OpenOCD's
        // `bp <address> 2 hw`).
        let ctrl = FpCtrl::from(core.read_word_32(FpCtrl::get_mmio_address())?);
        let comp: u32 = match ctrl.rev() {
            0 => FpRev1CompX::breakpoint_configuration(self.rom_breakpoint)?.into(),
            1 => FpRev2CompX::breakpoint_configuration(self.rom_breakpoint).into(),
            other => {
                return Err(ArmError::Other(format!(
                    "FPB revision {other} is not supported"
                )));
            }
        };
        core.write_word_32(FpRev1CompX::get_mmio_address(), comp)?;

        let mut ctrl = FpCtrl::from(0);
        ctrl.set_key(true);
        ctrl.set_enable(true);
        core.write_word_32(FpCtrl::get_mmio_address(), ctrl.into())?;

        // Clear stale sticky status bits.
        let _ = core.read_word_32(Dhcsr::get_mmio_address())?;

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Disable the FPB breakpoint.
        core.write_word_32(FpRev1CompX::get_mmio_address(), 0)?;
        let mut ctrl = FpCtrl::from(0);
        ctrl.set_key(true);
        ctrl.set_enable(false);
        core.write_word_32(FpCtrl::get_mmio_address(), ctrl.into())
    }

    /// Pulse nSRST (assert then release) so the DP is alive when the
    /// subsequent attach tries to connect.  Needed for `--connect-under-reset`.
    fn reset_hardware_assert(&self, interface: &mut dyn DapProbe) -> Result<(), ArmError> {
        let mut n_reset = Pins(0);
        n_reset.set_nreset(true);
        let mask = n_reset.0 as u32;

        interface.swj_pins(0, mask, 0)?;
        thread::sleep(Duration::from_millis(20));
        interface.swj_pins(mask, mask, 0)?;
        thread::sleep(Duration::from_millis(10));
        Ok(())
    }
}
