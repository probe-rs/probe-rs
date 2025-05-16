//! Sequences for tms570 devices
//!
//! The sequence used for catching a reset from Section 3 of the document
//! [JTAG Programmer Overview for Hercules-based Microcontrollers](https://www.ti.com/lit/an/spna230/spna230.pdf),
//! and largely involves creating a breakpoint, issuing some sort of reset,
//! then clearing memory using platform-specific register writes.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use crate::architecture::arm::armv7a::{
    clear_hw_breakpoint, get_hw_breakpoint, read_word_32, request_halt, run, set_hw_breakpoint,
    wait_for_core_halted, write_word_32,
};
use crate::architecture::arm::communication_interface::DapProbe;
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::{ArmDebugSequence, ArmDebugSequenceError};
use crate::architecture::arm::{ArmError, dp::DpAddress};
use crate::probe::WireProtocol;

use super::icepick::Icepick;

const TMS570_TAP_INDEX: u8 = 0;

/// How long to wait for the core to halt.
const HALT_DELAY: Duration = Duration::from_millis(100);

/// Marker struct indicating initialization sequencing for cc13xx_cc26xx family parts.
#[derive(Debug)]
pub struct TMS570 {
    existing_breakpoint: AtomicU32,
    breakpoint_active: AtomicBool,
}

impl TMS570 {
    /// Create the sequencer for the cc13xx_cc26xx family of parts.
    pub fn create(_name: String) -> Arc<Self> {
        Arc::new(Self {
            existing_breakpoint: AtomicU32::new(0),
            breakpoint_active: AtomicBool::new(false),
        })
    }
}

impl ArmDebugSequence for TMS570 {
    /// When a core is reset, it is always set to be caught
    fn reset_catch_set(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let base_address = debug_base.ok_or(ArmError::NoArmTarget)?;

        // Halt the core. Note that this bypasses any sort of register cache,
        // and doesn't update the status within the ARMv7A.
        request_halt(memory, base_address)?;
        wait_for_core_halted(memory, base_address, HALT_DELAY)?;

        // If there is an existing breakpoint at address 0, note that down before
        // replacing it with our own breakpoint.
        let existing = get_hw_breakpoint(memory, base_address, 0)?;
        if let Some(existing) = existing {
            self.existing_breakpoint.store(existing, Ordering::Release);
        }
        self.breakpoint_active
            .store(existing.is_some(), Ordering::Relaxed);

        // Insert a breakpoint at address 0
        set_hw_breakpoint(memory, base_address, 0, 0)?;
        run(memory, base_address)?;

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let base_address = debug_base.ok_or(ArmError::NoArmTarget)?;

        // Halt the core. Note that this bypasses any sort of register cache,
        // and doesn't update the status within the ARMv7A.
        request_halt(memory, base_address)?;
        wait_for_core_halted(memory, base_address, HALT_DELAY)?;

        if self.breakpoint_active.swap(false, Ordering::Release) {
            set_hw_breakpoint(
                memory,
                base_address,
                0,
                self.existing_breakpoint.load(Ordering::Relaxed),
            )?;
        } else {
            clear_hw_breakpoint(memory, base_address, 0)?;
        }

        // TMS570 has ECC RAM. Ensure it's cleared to avoid cascading failures. Without
        // this, writes to SRAM will trap, preventing execution from RAM.
        write_word_32(memory, base_address, 0xffff_ff5c, 0xau32)?;
        write_word_32(memory, base_address, 0xffff_ff60, 1u32)?;
        while read_word_32(memory, base_address, 0xffff_ff68)? & (1u32 << 8) == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        write_word_32(memory, base_address, 0xffff_ff5c, 0x5u32)?;

        Ok(())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let arm_probe = interface.get_arm_probe_interface()?;
        let probe = arm_probe.try_dap_probe_mut().ok_or(ArmError::NoArmTarget)?;
        let mut icepick = Icepick::initialized(probe)?;
        icepick.sysreset()?;
        icepick.bypass()?;

        Ok(())
    }

    fn debug_port_setup(
        &self,
        interface: &mut dyn DapProbe,
        _dp: DpAddress,
    ) -> Result<(), ArmError> {
        // Ensure current debug interface is in reset state.
        interface.swj_sequence(51, 0x0007_FFFF_FFFF_FFFF)?;

        match interface.active_protocol() {
            Some(WireProtocol::Jtag) => {
                let mut icepick = Icepick::new(interface)?;
                icepick.select_tap(TMS570_TAP_INDEX)?;

                // Call the configure JTAG function. We don't derive the scan chain at runtime
                // for these devices, but regardless the scan chain must be told to the debug probe
                // We avoid the live scan for the following reasons:
                // 1. Only the ICEPICK is connected at boot so we need to manually the CPU to the scan chain
                // 2. Entering test logic reset disconects the CPU again
                interface.configure_jtag(true)?;
            }
            Some(WireProtocol::Swd) => {
                return Err(ArmDebugSequenceError::SequenceSpecific(
                    "The tms570 family doesn't support SWD".into(),
                )
                .into());
            }
            _ => {
                return Err(ArmDebugSequenceError::SequenceSpecific(
                    "Cannot detect current protocol".into(),
                )
                .into());
            }
        }

        Ok(())
    }
}
