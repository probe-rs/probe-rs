//! Implement chip-specific dual-core reset for RP2040

use std::sync::Arc;

use crate::{
    MemoryMappedRegister,
    architecture::arm::{
        ArmError,
        armv6m::{Aircr, Demcr},
        dp::{Ctrl, DpAddress, DpRegister},
        memory::ArmMemoryInterface,
        sequences::ArmDebugSequence,
    },
};

const SIO_CPUID_OFFSET: u64 = 0xd000_0000;
const RESCUE_DP: DpAddress = DpAddress::Multidrop(0xf100_2927);

/// Debug implementation for RP2040
#[derive(Debug)]
pub struct Rp2040;

impl Rp2040 {
    /// Create a debug sequencer for a Raspberry Pi RP2040
    pub fn create() -> Arc<Self> {
        Arc::new(Rp2040)
    }
}

impl ArmDebugSequence for Rp2040 {
    fn reset_system(
        &self,
        core: &mut dyn ArmMemoryInterface,
        core_type: crate::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Only perform a system reset from core 0
        let core_id = core.read_word_32(SIO_CPUID_OFFSET)?;
        if core_id != 0 {
            tracing::debug!("Skipping reset of core {core_id}");
            return Ok(());
        }

        // Since we're resetting the core, the catch_reset flag will get lost.
        // Note whether we should re-set it after entering Rescue Mode.
        let should_catch_reset =
            Demcr(core.read_word_32(Demcr::get_mmio_address())?).vc_corereset();

        // Put the core into Rescue Mode. Do this by poking the Rescue Core on the
        // SWD multidrop bus and then clearing the DBGPWRUPREQ flag in the
        // VREG_AND_POR_CHIP_RESET register. This will reset both cores
        // and leave core 0 in a state where it can run debug code.
        // For more information, see 2.3.4.2 in the RP2040 Datasheet:
        // <https://datasheets.raspberrypi.com/rp2040/rp2040-datasheet.pdf>

        let ap = core.fully_qualified_address();
        let arm_interface = core.get_arm_debug_interface()?;

        arm_interface.write_raw_dp_register(RESCUE_DP, Ctrl::ADDRESS, 0)?;

        // Start the debug core back up which brings it out of Rescue Mode
        self.debug_core_start(arm_interface, &ap, core_type, debug_base, None)?;

        // If we were set to catch the reset before, set it up again
        if should_catch_reset {
            self.reset_catch_set(core, core_type, debug_base)?
        }

        // Perform a reset to get out of Rescue Mode by poking AIRCR.
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        core.write_word_32(Aircr::get_mmio_address(), aircr.into())?;

        Ok(())
    }
}
