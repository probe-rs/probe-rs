//! Implement chip-specific dual-core reset for RP2040

use std::sync::Arc;

use crate::{
    MemoryMappedRegister,
    architecture::arm::{
        ArmError,
        armv6m::{Aircr, Demcr},
        dp::{Ctrl, DpAddress, DpRegister},
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, cortex_m_wait_for_reset},
    },
};

const SIO_CPUID_OFFSET: u64 = 0xd000_0000;
const RESCUE_DP: DpAddress = DpAddress::Multidrop(0xf100_2927);

// Performing a reset causes all cores to reset their SWD connections. The reset
// takes place on core 0, however we still need to restore the core 1 protocol
// characteristics such as overrun detection. To do this, we need to manually
// specify the core 1 DP address.
const CORE_1_DP: DpAddress = DpAddress::Multidrop(0x1100_2927);

/// An empty struct that implements the default [ArmDebugSequence] methods
/// to allow us to call default implementations from our derived function.
#[derive(Debug)]
struct DefaultArmDebugSequence;
impl ArmDebugSequence for DefaultArmDebugSequence {}

/// Debug implementation for RP2040
#[derive(Debug)]
pub struct Rp2040 {}

impl Rp2040 {
    /// Create a debug sequencer for a Raspberry Pi RP2040
    pub fn create() -> Arc<Self> {
        Arc::new(Rp2040 {})
    }
}

impl ArmDebugSequence for Rp2040 {
    fn debug_port_setup(
        &self,
        interface: &mut dyn crate::architecture::arm::communication_interface::DapProbe,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        // Before we set up the requested DP, start and then stop the Default DP.
        // This works around an issue where the multidrop DP becomes unavailable.
        if DefaultArmDebugSequence
            .debug_port_setup(interface, DpAddress::Default)
            .is_err()
        {
            tracing::error!("Unable to connect to default address");
        }
        self.debug_port_stop(interface, DpAddress::Default)?;

        // Delegate actual debug port setup to the default implementation.
        DefaultArmDebugSequence.debug_port_setup(interface, dp)?;
        Ok(())
    }

    fn reset_system(
        &self,
        core: &mut dyn ArmMemoryInterface,
        core_type: crate::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::trace!("Starting reset of RP2040");
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

        // Take note of the existing values for CTRL. We will need to restore these after entering
        // rescue mode.
        let existing_core_0 = arm_interface.read_raw_dp_register(ap.dp(), Ctrl::ADDRESS)?;
        let existing_core_1 = arm_interface.read_raw_dp_register(CORE_1_DP, Ctrl::ADDRESS)?;

        // Perform the reset by poking the rescue DP
        arm_interface.write_raw_dp_register(RESCUE_DP, Ctrl::ADDRESS, 0)?;
        tracing::trace!(
            "Existing values core0: {existing_core_0:08x}  core1: {existing_core_1:08x}"
        );

        // The debug port is reset as well. Set it up again by sending the attention sequence again
        let dap_probe = arm_interface.try_dap_probe_mut().unwrap();

        // Run the setup sequence again, which will reacquire the multidrop target.
        self.debug_port_setup(dap_probe, ap.dp())?;

        // Start the debug core back up which brings it out of Rescue Mode
        self.debug_core_start(arm_interface, &ap, core_type, debug_base, None)?;

        // Restore the ctrl values
        tracing::trace!("Restoring ctrl values");
        arm_interface.write_raw_dp_register(ap.dp(), Ctrl::ADDRESS, existing_core_0)?;
        arm_interface.write_raw_dp_register(CORE_1_DP, Ctrl::ADDRESS, existing_core_1)?;

        // If we were set to catch the reset before, set it up again
        if should_catch_reset {
            tracing::trace!("Re-setting reset catch");
            self.reset_catch_set(core, core_type, debug_base)?
        }

        // Perform a reset to get out of Rescue Mode by poking AIRCR.
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        core.write_word_32(Aircr::get_mmio_address(), aircr.into())?;

        // Wait for the core to finish resetting
        cortex_m_wait_for_reset(core)?;

        tracing::trace!("Finished RP2040 reset sequence");
        Ok(())
    }
}
