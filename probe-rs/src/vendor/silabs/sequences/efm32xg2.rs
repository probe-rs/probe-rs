//! Sequences for Silicon Labs EFM32 Series 2 chips

use std::sync::Arc;

use crate::{
    architecture::arm::{
        core::armv7m::{Demcr, Dhcsr},
        memory::adi_v5_memory_interface::ArmMemoryInterface,
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
}
