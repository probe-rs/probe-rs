//! Sequences for Silicon Labs EFM32 Series 2 chips

use std::sync::Arc;

use crate::{
    architecture::arm::{
        ArmError,
        core::armv8m::{Aircr, Demcr, Dhcsr},
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, cortex_m_wait_for_reset},
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
}

#[async_trait::async_trait(?Send)]
impl ArmDebugSequence for EFM32xG2 {
    async fn reset_catch_set(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let reset_vector = core.read_word_32(0x0000_0004).await?;

        if reset_vector != 0xffff_ffff {
            tracing::info!("Breakpoint on user application reset vector");
            core.write_word_32(0xE000_2008, reset_vector | 1).await?;
            core.write_word_32(0xE000_2000, 3).await?;
        }

        if reset_vector == 0xffff_ffff {
            tracing::info!("Enable reset vector catch");
            let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address()).await?);
            demcr.set_vc_corereset(true);
            core.write_word_32(Demcr::get_mmio_address(), demcr.into())
                .await?;
        }

        let _ = core.read_word_32(Dhcsr::get_mmio_address()).await?;

        Ok(())
    }

    async fn reset_catch_clear(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        core.write_word_32(0xE000_2008, 0x0).await?;
        core.write_word_32(0xE000_2000, 0x2).await?;

        let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address()).await?);
        demcr.set_vc_corereset(false);
        core.write_word_32(Demcr::get_mmio_address(), demcr.into())
            .await
    }

    async fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        interface
            .write_word_32(Aircr::get_mmio_address(), aircr.into())
            .await?;
        cortex_m_wait_for_reset(interface).await?;

        let dhcsr = Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address()).await?);
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
            interface
                .write_word_32(Dhcsr::get_mmio_address(), value.into())
                .await?;

            // Request halt-on-reset.
            let mut demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address()).await?);
            demcr.set_vc_corereset(true);
            interface
                .write_word_32(Demcr::get_mmio_address(), demcr.into())
                .await?;

            // Trigger reset.
            let mut aircr = Aircr(0);
            aircr.vectkey();
            aircr.set_vectreset(true);
            aircr.set_vectclractive(true);
            interface
                .write_word_32(Aircr::get_mmio_address(), aircr.into())
                .await?;

            cortex_m_wait_for_reset(interface).await?;

            // We should no longer be in lokup state at this point. CoreInterface::status is going
            // to chek this soon.
        }

        Ok(())
    }
}
