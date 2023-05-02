//! Sequences for Nrf52 devices

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::ArmDebugSequence;
use crate::architecture::arm::memory::adi_v5_memory_interface::ArmProbe;
use crate::architecture::arm::ArmError;

/// Marker struct indicating initialization sequencing for RP2040 family parts.
pub struct Rp2040 {}

impl Rp2040 {
    /// Create the sequencer for the RP2040 family of parts.
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }
}

const PSM_FRCE_ON: u64 = 0x40010000;
const PSM_FRCE_OFF: u64 = 0x40010004;
const PSM_WDSEL: u64 = 0x40010008;

const PSM_SEL_SIO: u32 = 1 << 14;
const PSM_SEL_PROC0: u32 = 1 << 15;
const PSM_SEL_PROC1: u32 = 1 << 16;

const WATCHDOG_CTRL: u64 = 0x40058000;
const WATCHDOG_CTRL_TRIGGER: u32 = 1 << 31;
const WATCHDOG_CTRL_ENABLE: u32 = 1 << 30;

impl ArmDebugSequence for Rp2040 {
    fn reset_system(
        &self,
        interface: &mut dyn ArmProbe,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::debug!("rp2040: resetting SIO and processors");
        interface.write_word_32(PSM_WDSEL, PSM_SEL_SIO | PSM_SEL_PROC0 | PSM_SEL_PROC1)?;
        interface.write_word_32(WATCHDOG_CTRL, WATCHDOG_CTRL_ENABLE)?;
        interface.write_word_32(WATCHDOG_CTRL, WATCHDOG_CTRL_ENABLE | WATCHDOG_CTRL_TRIGGER)?;

        // random sleep. No idea if this is needed.
        thread::sleep(Duration::from_millis(100));

        tracing::debug!("rp2040: reset done");

        interface
            .get_arm_communication_interface()
            .unwrap()
            .clear_state();

        Ok(())
    }
}
