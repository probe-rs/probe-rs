//! Sequences for NXP chips that use ARMv7-M cores.

use crate::architecture::arm::armv6m::{Aircr, BpCompx, BpCtrl, Demcr, Dhcsr};
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::ArmDebugSequence;
use crate::architecture::arm::ArmError;
use crate::core::MemoryMappedRegister;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The sequence handle for the LPC80x family.
#[derive(Debug)]
pub struct LPC80x(());

impl LPC80x {
    /// Create a sequence handle for the LPC80x.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }
}

fn force_core_halt(interface: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    tracing::trace!("force_core_halt enter");

    let start = Instant::now();
    let mut dhcsr = interface.read_word_32(Dhcsr::get_mmio_address()).unwrap() & 0x20000;
    while start.elapsed() < Duration::from_millis(100) && dhcsr == 0 {
        dhcsr = interface.read_word_32(Dhcsr::get_mmio_address()).unwrap() & 0x20000;
    }
    // if dhcsr & 0x20000 is still 0 we hit the timeout, try halting again.
    if dhcsr == 0 {
        interface.write_word_32(Dhcsr::get_mmio_address(), 0xA05F0003)?;
        let start = Instant::now();
        let mut dhcsr = interface.read_word_32(Dhcsr::get_mmio_address()).unwrap() & 0x20000;
        while start.elapsed() < Duration::from_millis(1) && dhcsr == 0 {
            dhcsr = interface.read_word_32(Dhcsr::get_mmio_address()).unwrap() & 0x20000;
        }
    }

    tracing::trace!("force_core_halt exit");
    Ok(())
}

impl ArmDebugSequence for LPC80x {
    fn reset_catch_set(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        const FPB_BKPT_H: u32 = 0x80000000;
        const FPB_BKPT_L: u32 = 0x40000000;
        const FPB_COMP_M: u32 = 0x1FFFFFFC;
        const FPB_KEY: u32 = 0x00000002;
        const FPB_ENABLE: u32 = 0x00000001;
        tracing::trace!("reset_catch_set enter");

        // Disable Reset Vector Catch in DEMCR
        let demcr = interface.read_word_32(Demcr::get_mmio_address())?;
        interface.write_word_32(Demcr::get_mmio_address(), demcr & !0x00000001)?;

        // Map Flash to Vectors
        interface.write_word_32(0x4004_8000, 0x0000_0002)?;

        // Read reset vector from Flash
        let reset_vector = interface.read_word_32(0x0000_0004)?;
        tracing::info!("Reset Vector is address 0x{:08x}", reset_vector);

        let bp_match = if (reset_vector & 0x02) != 0 {
            FPB_BKPT_H
        } else {
            FPB_BKPT_L
        };

        // Set BP0 to Reset Vector
        let bpcompx = bp_match | (reset_vector & FPB_COMP_M) | FPB_ENABLE;
        interface.write_word_32(BpCompx::get_mmio_address(), bpcompx)?;
        // Enable FPB
        interface.write_word_32(BpCtrl::get_mmio_address(), FPB_KEY | FPB_ENABLE)?;

        // Clear the status bits by reading from DHCSR
        let _ = interface.read_word_32(Dhcsr::get_mmio_address())?;
        tracing::trace!("reset_catch_set exit");

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::trace!("reset_catch_clear enter");

        // Disable Reset Vector Catch in DEMCR
        let d = interface.read_word_32(Demcr::get_mmio_address())? & !0x00000001;
        interface.write_word_32(Demcr::get_mmio_address(), d)?;
        // Clear BP0
        interface.write_word_32(0xE000_2008, 0x0)?;
        // Disable FPB
        interface.write_word_32(0xE000_2000, 0x2)?;

        tracing::debug!("reset_catch_clear exit");
        Ok(())
    }

    /// ResetSystem for Cortex-M devices
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::debug!("reset_system enter");

        // Execute VECTRESET via AIRCR, ignore errors.
        let _ = interface.write_32(Aircr::get_mmio_address(), &[0x05FA0004]);

        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(100) {
            if let Ok(dhcr) = interface.read_word_32(Dhcsr::get_mmio_address()) {
                if dhcr & 0x20000 != 0 {
                    break;
                }
            }
        }

        let _ = force_core_halt(interface);
        tracing::debug!("reset_system exit");
        return Ok(());
    }
}
