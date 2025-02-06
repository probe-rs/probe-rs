//! Sequences for Silicon Labs EFM32 Series 2 chips

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{
    architecture::arm::{
        ap::AccessPortError,
        core::armv8m::{Aircr, Demcr, Dhcsr},
        memory::ArmMemoryInterface,
        sequences::ArmDebugSequence,
        ArmError,
    },
    core::MemoryMappedRegister,
};

/// The sequence handle for the EFM32 Series 2 family.
///
/// Uses a breakpoint on the reset vector for the reset catch.
#[derive(Debug)]
pub struct EFM32xG2(());

impl EFM32xG2 {
    /// Create a sequence handle for the EFM32xG2
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }

    fn wait_for_reset(&self, interface: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        let start = Instant::now();

        while start.elapsed() < Duration::from_millis(500) {
            let dhcsr = match interface.read_word_32(Dhcsr::get_mmio_address()) {
                Ok(val) => Dhcsr(val),
                // Some combinations of debug probe and target (in
                // particular, hs-probe and ATSAMD21) result in
                // register read errors while the target is
                // resetting.
                Err(ArmError::AccessPort {
                    source: AccessPortError::RegisterRead { .. },
                    ..
                }) => continue,
                Err(err) => return Err(err),
            };
            if !dhcsr.s_reset_st() {
                return Ok(());
            }
        }

        Err(ArmError::Timeout)
    }
}

impl ArmDebugSequence for EFM32xG2 {
    fn reset_catch_set(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let reset_vector = core.read_word_32(0x0000_0004)?;

        if reset_vector != 0xffff_ffff {
            tracing::info!("Breakpoint on user application reset vector");
            core.write_word_32(0xE000_2008, reset_vector | 1)?;
            core.write_word_32(0xE000_2000, 3)?;
        }

        if reset_vector == 0xffff_ffff {
            tracing::info!("Enable reset vector catch");
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
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        core.write_word_32(0xE000_2008, 0x0)?;
        core.write_word_32(0xE000_2000, 0x2)?;

        let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
        demcr.set_vc_corereset(false);
        core.write_word_32(Demcr::get_mmio_address(), demcr.into())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        interface.write_word_32(Aircr::get_mmio_address(), aircr.into())?;
        self.wait_for_reset(interface)?;

        let dhcsr = Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address())?);
        if dhcsr.s_lockup() {
            // Try to resolve lockup by halting the core again with a modified version of SiLab's
            // application note AN0062 'Programming Internal Flash Over the Serial Wire Debug
            // Interface', section 3.1 'Halting the CPU'
            // (https://www.silabs.com/documents/public/application-notes/an0062.pdf).
            //
            // Using just SYSRESETREQ did not work for mass-erased EFM32xG2/Cortex-M33 devices. But
            // using VECTRESET instead, like OpenOCD documents as its default and as it can be seen
            // from Simplicity Commander, does the trick.

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

            self.wait_for_reset(interface)?;

            // We should no longer be in lokup state at this point. CoreInterface::status is going
            // to chek this soon.
        }

        Ok(())
    }
}
