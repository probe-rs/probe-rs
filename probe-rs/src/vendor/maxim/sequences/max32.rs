//! Debug sequences for Maxim Integrated MAX32xxx microcontrollers (Cortex-M4, ARMv7-M).
//!
//! Uses VECTRESET (core-only reset via AIRCR) instead of SYSRESETREQ.
//! VECTRESET resets the processor without touching the Debug Port, so the
//! DP connection stays alive through the reset.
//!
//! nSRST (hardware pin reset) is also available via `--connect-under-reset`.

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use probe_rs_target::CoreType;

use crate::{
    architecture::arm::{
        ArmDebugInterface, ArmError, DapProbe, FullyQualifiedApAddress, Pins,
        core::armv7m::{Aircr, Demcr, Dhcsr},
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, cortex_m_wait_for_reset},
    },
    core::MemoryMappedRegister,
};

/// Debug sequence for Maxim MAX32xxx chips that require VECTRESET.
#[derive(Debug, Clone)]
pub struct Max32;

impl Max32 {
    /// Creates a new [`Max32`] debug sequence.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self)
    }
}

impl ArmDebugSequence for Max32 {
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

    /// Reset was already released in `reset_hardware_assert`. Ensure nRESET is
    /// high, then manually halt the core via DHCSR in a retry loop.
    fn reset_hardware_deassert(
        &self,
        probe: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let mut n_reset = Pins(0);
        n_reset.set_nreset(true);
        let n_reset = n_reset.0 as u32;
        let _ = probe.swj_pins(n_reset, n_reset, 0)?;

        let mut memory = probe.memory_interface(default_ap)?;

        let mut request_halt = Dhcsr(0);
        request_halt.set_c_halt(true);
        request_halt.set_c_debugen(true);
        request_halt.enable_write();

        let start = Instant::now();
        loop {
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

    /// Use VECTRESET for MAX32690 instead of the default SYSRESETREQ.
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);
        demcr.set_vc_corereset(true);
        interface.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_vectreset(true);
        aircr.set_vectclractive(true);
        interface.write_word_32(Aircr::get_mmio_address(), aircr.into())?;

        cortex_m_wait_for_reset(interface)?;

        Ok(())
    }
}
