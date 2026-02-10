//! Sequences for Silicon Labs EFM32 Series 2 chips

use std::{
    fmt::Debug,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use probe_rs_target::Chip;

use crate::{
    architecture::arm::{
        ArmError, FullyQualifiedApAddress,
        core::armv8m::{Aircr, Demcr, Dhcsr},
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, DebugEraseSequence, cortex_m_wait_for_reset},
    },
    core::MemoryMappedRegister,
};

/// The sequence handle for the EFM32 Series 2 family.
///
/// Uses a breakpoint on the reset vector for the reset catch.
#[derive(Debug, Clone)]
pub struct EFM32xG2 {
    flash_base_addr: u64,
    use_msc_erase: bool,
}

impl EFM32xG2 {
    /// Create a sequence handle for the EFM32xG2
    pub fn create(chip: &Chip) -> Arc<dyn ArmDebugSequence> {
        let is_series_2c3 = chip.name.starts_with("EFR32FG23")
            || chip.name.starts_with("EFR32MG24")
            || chip.name.starts_with("EFR32PG26");

        let flash_base_addr = if is_series_2c3 { 0x0800_0000 } else { 0 };

        Arc::new(Self {
            flash_base_addr,
            use_msc_erase: is_series_2c3,
        })
    }
}

impl ArmDebugSequence for EFM32xG2 {
    fn reset_catch_set(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let reset_vector = core.read_word_32(self.flash_base_addr + 0x4)?;

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
        cortex_m_wait_for_reset(interface)?;

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

            cortex_m_wait_for_reset(interface)?;

            // We should no longer be in lokup state at this point. CoreInterface::status is going
            // to chek this soon.
        }

        Ok(())
    }

    fn debug_erase_sequence(&self) -> Option<Arc<dyn DebugEraseSequence>> {
        if self.use_msc_erase {
            Some(Arc::new(MscEraseSequence {}))
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub(crate) struct MscEraseSequence;

impl DebugEraseSequence for MscEraseSequence {
    fn erase_all(
        &self,
        interface: &mut dyn crate::architecture::arm::ArmDebugInterface,
    ) -> Result<(), ArmError> {
        let mut mem =
            interface.memory_interface(&FullyQualifiedApAddress::v1_with_default_dp(0))?;

        const CMU_BASE: u64 = 0x4000_8000;
        const CMU_CLKEN1_SET: u64 = CMU_BASE + 0x1068;
        const CMU_CLKEN1_SET_MSC: u32 = 1 << 16;

        // Enable MSC clock
        mem.write_word_32(CMU_CLKEN1_SET, CMU_CLKEN1_SET_MSC)?;

        const MSC_BASE: u64 = 0x4003_0000;
        const MSC_WRITECTRL: u64 = MSC_BASE + 0x0C;
        const MSC_WRITECTRL_WREN: u32 = 1;
        const MSC_WRITECMD: u64 = MSC_BASE + 0x10;
        const MSC_WRITECMD_ERASEMAIN0: u32 = 1 << 8;
        const MSC_STATUS: u64 = MSC_BASE + 0x1C;
        const MSC_STATUS_BUSY: u32 = 1;

        // Enable flash write/erase
        mem.write_word_32(MSC_WRITECTRL, MSC_WRITECTRL_WREN)?;

        // Initiate mass erase
        mem.write_word_32(MSC_WRITECMD, MSC_WRITECMD_ERASEMAIN0)?;

        // Poll status until erase is complete
        let start = Instant::now();
        loop {
            let status = mem.read_word_32(MSC_STATUS)?;
            if status & MSC_STATUS_BUSY == 0 {
                break;
            }

            if start.elapsed().as_millis() > 2000 {
                Err(ArmError::Timeout)?;
            }

            thread::sleep(Duration::from_millis(10));
        }

        Ok(())
    }
}
