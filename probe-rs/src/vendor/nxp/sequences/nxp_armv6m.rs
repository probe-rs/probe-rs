//! Sequences for NXP chips that use ARMv6-M cores.

use crate::architecture::arm::ArmError;
use crate::architecture::arm::armv6m::{Aircr, Demcr, Dhcsr};
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::ArmDebugSequence;
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

    // copy-paste of set_hw_breakpoint since we don't have access to core :(
    fn set_hw_breakpoint(
        interface: &mut dyn ArmMemoryInterface,
        bp_register_index: usize,
        addr: u32,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::armv6m::BpCompx;
        tracing::trace!(
            "Setting breakpoint in lpc804 sequence on address 0x{:08x}",
            addr
        );
        let mut value = BpCompx(0);
        if addr % 4 < 2 {
            // match lower halfword
            value.set_bp_match(0b01);
        } else {
            // match higher halfword
            value.set_bp_match(0b10);
        }
        value.set_comp((addr >> 2) & 0x07FF_FFFF);
        value.set_enable(true);

        let register_addr =
            BpCompx::get_mmio_address() + (bp_register_index * size_of::<u32>()) as u64;
        interface.write_word_32(register_addr, value.into())?;

        Ok(())
    }

    // copy-paste of clear_hw_breakpoint since we don't have access to core :(
    fn clear_hw_breakpoint(
        interface: &mut dyn ArmMemoryInterface,
        bp_unit_index: usize,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::armv6m::BpCompx;
        tracing::trace!("Clearing breakpoint in lpc804 sequence ");
        let register_addr = BpCompx::get_mmio_address() + (bp_unit_index * size_of::<u32>()) as u64;

        let mut value = BpCompx::from(0);
        value.set_enable(false);

        interface.write_word_32(register_addr, value.into())?;

        Ok(())
    }

    // custom core halt logic from cmsis-pack sequence
    fn force_core_halt(interface: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        tracing::span!(tracing::Level::TRACE, "force_core_halt");

        let start = Instant::now();
        let mut in_debug_state = Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address())?).s_halt();
        while start.elapsed() < Duration::from_millis(100) && !in_debug_state {
            in_debug_state = Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address())?).s_halt();
        }
        // if dhcsr & 0x20000 (s_halt) is still 0 and we hit the above timeout, try halting again.
        if !in_debug_state {
            let mut dhcsr = Dhcsr(0);
            dhcsr.set_c_halt(true);
            dhcsr.set_c_debugen(true);
            dhcsr.enable_write();
            interface.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(100) {
                if Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address())?).s_halt() {
                    break;
                };
            }
        }

        Ok(())
    }
}

impl ArmDebugSequence for LPC80x {
    fn reset_catch_set(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::span!(tracing::Level::TRACE, "reset_catch_set");

        // Disable Reset Vector Catch in DEMCR
        let mut demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);
        demcr.set_vc_corereset(false);
        interface.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

        // Map Flash to Vectors
        interface.write_word_32(0x4004_8000, 0x0000_0002)?;

        // Read reset vector from Flash
        let reset_vector = interface.read_word_32(0x0000_0004)?;
        tracing::trace!("Reset Vector is address 0x{:08x}", reset_vector);

        LPC80x::set_hw_breakpoint(interface, 0, reset_vector)?;

        // Clear the status bits by reading from DHCSR
        let _ = interface.read_word_32(Dhcsr::get_mmio_address())?;

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::span!(tracing::Level::TRACE, "reset_catch_clear");

        // Disable Reset Vector Catch in DEMCR
        let mut demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);
        demcr.set_vc_corereset(false);
        interface.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

        LPC80x::clear_hw_breakpoint(interface, 0)?;

        Ok(())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::span!(tracing::Level::TRACE, "reset_system enter");

        // Execute VECTRESET via AIRCR, ignore errors.
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        let _ = interface.write_32(Aircr::get_mmio_address(), &[aircr.0]);

        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(100) {
            // ignore read errors while resetting
            if let Ok(dhcr) = interface.read_word_32(Dhcsr::get_mmio_address())
                && Dhcsr(dhcr).s_halt()
            {
                // return early if we're in debug state
                return Ok(());
            }
        }

        let _ = LPC80x::force_core_halt(interface);

        Ok(())
    }
}
