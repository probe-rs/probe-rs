pub mod nxp;

use std::{
    thread,
    time::{Duration, Instant},
};

use crate::{core::CoreRegister, DebugProbeError, Memory};

use super::{
    communication_interface::{SwdSequence, UninitializedArmProbe},
    dp::{Abort, Ctrl, DpAccess, Select},
};

pub struct DefaultArmSequence;

impl ArmDebugSequence for DefaultArmSequence {}

pub trait ArmDebugSequence: Send + Sync {
    /// Implementation of the debug sequence `ResetHardwareAssert` from the ARM debug sequences.
    #[doc(alias = "ResetHardwareAssert")]
    fn reset_hardware_assert(&self, memory: &mut Memory) -> Result<(), crate::Error> {
        let interface = memory.get_arm_interface()?;

        let n_reset = 0x80;

        let _ = interface.swj_pins(0, n_reset, 0)?;

        Ok(())
    }

    fn reset_hardware_deassert(&self, memory: &mut Memory) -> Result<(), crate::Error> {
        let interface = memory.get_arm_interface()?;

        let n_reset = 0x80;

        let can_read_pins = interface.swj_pins(n_reset, n_reset, 0)? != 0xffff_ffff;

        if can_read_pins {
            let start = Instant::now();

            while start.elapsed() < Duration::from_secs(1) {
                if interface.swj_pins(n_reset, n_reset, 0)? & n_reset != 0 {
                    return Ok(());
                }
            }

            Err(DebugProbeError::Timeout.into())
        } else {
            thread::sleep(Duration::from_millis(100));
            Ok(())
        }
    }

    /// Implementation of the debug sequence *DebugPortSetup* from CMSIS Pack debug sequences.
    #[doc(alias = "DebugPortSetup")]
    fn debug_port_setup(
        &self,
        interface: &mut Box<dyn UninitializedArmProbe>,
    ) -> Result<(), crate::Error> {
        // TODO: Handle this differently for ST-Link?

        // TODO: Use atomic block

        // Ensure current debug interface is in reset state
        interface.swj_sequence(51, 0x0007_FFFF_FFFF_FFFF)?;

        // Execute SWJ-DP Switch Sequence JTAG to SWD (0xE79E)
        // Change if SWJ-DP uses deprecated switch code (0xEDB6)
        interface.swj_sequence(16, 0xE79E)?;

        interface.swj_sequence(51, 0x0007_FFFF_FFFF_FFFF)?; // > 50 cycles SWDIO/TMS High
        interface.swj_sequence(3, 0x00)?; // At least 2 idle cycles (SWDIO/TMS Low)

        // End of atomic block

        // Read DPIDR to enable SWD interface
        interface.read_dpidr()?;

        Ok(())
    }

    fn debug_port_start(&self, memory: &mut Memory) -> Result<(), crate::Error> {
        let interface = memory.get_arm_interface()?;

        interface
            .write_dp_register(Select(0))
            .map_err(DebugProbeError::from)?;

        //let powered_down = interface.read_dp_register::<Select>::()

        let ctrl = interface
            .read_dp_register::<Ctrl>()
            .map_err(DebugProbeError::from)?;

        let powered_down = !(ctrl.csyspwrupack() && ctrl.cdbgpwrupack());

        if powered_down {
            let mut ctrl = Ctrl(0);
            ctrl.set_cdbgpwrupreq(true);
            ctrl.set_csyspwrupreq(true);

            interface
                .write_dp_register(ctrl)
                .map_err(DebugProbeError::from)?;

            let start = Instant::now();

            let mut timeout = true;

            while start.elapsed() < Duration::from_micros(100_0000) {
                let ctrl = interface
                    .read_dp_register::<Ctrl>()
                    .map_err(DebugProbeError::from)?;

                if ctrl.csyspwrupack() && ctrl.cdbgpwrupack() {
                    timeout = false;
                    break;
                }
            }

            if timeout {
                return Err(crate::Error::Probe(DebugProbeError::Timeout));
            }

            // TODO: Handle JTAG Specific part

            // TODO: Only run the following code when the SWD protocol is used

            // Init AP Transfer Mode, Transaction Counter, and Lane Mask (Normal Transfer Mode, Include all Byte Lanes)
            let mut ctrl = Ctrl(0);

            ctrl.set_cdbgpwrupreq(true);
            ctrl.set_csyspwrupreq(true);

            ctrl.set_mask_lane(0b1111);

            interface
                .write_dp_register(ctrl)
                .map_err(DebugProbeError::from)?;

            let mut abort = Abort(0);

            abort.set_orunerrclr(true);
            abort.set_wderrclr(true);
            abort.set_stkerrclr(true);
            abort.set_stkcmpclr(true);

            interface
                .write_dp_register(abort)
                .map_err(DebugProbeError::from)?;
        }

        Ok(())
    }

    /// Enable debugging on an ARM core. This is based on the
    /// `DebugCoreStart` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#debugCoreStart
    fn debug_core_start(&self, core: &mut Memory) -> Result<(), crate::Error> {
        use crate::architecture::arm::core::m4::Dhcsr;

        let current_dhcsr = Dhcsr(core.read_word_32(Dhcsr::ADDRESS)?);

        // Note: Manual addition for debugging, not part of the original DebugCoreStart function
        if current_dhcsr.c_debugen() {
            log::debug!("Core is already in debug mode, no need to enable it again");
            return Ok(());
        }
        // -- End addition

        let mut dhcsr = Dhcsr(0);
        dhcsr.set_c_debugen(true);
        dhcsr.enable_write();

        core.write_word_32(Dhcsr::ADDRESS, dhcsr.into())?;

        Ok(())
    }

    /// Setup the core to stop after reset. After this, the core will halt when it comes
    /// out of reset. This is based on the `ResetCatchSet` function from
    /// the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#resetCatchSet
    fn reset_catch_set(&self, core: &mut Memory) -> Result<(), crate::Error> {
        use crate::architecture::arm::core::m4::{Demcr, Dhcsr};

        // Request halt after reset
        let mut demcr = Demcr(core.read_word_32(Demcr::ADDRESS)?);
        demcr.set_vc_corereset(true);

        core.write_word_32(Demcr::ADDRESS, demcr.into())?;

        // Clear the status bits by reading from DHCSR
        let _ = core.read_word_32(Dhcsr::ADDRESS)?;

        Ok(())
    }

    /// Undo the settings of the `reset_catch_set` function.
    /// This is based on the `ResetCatchSet` function from
    /// the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#resetCatchClear
    fn reset_catch_clear(&self, core: &mut Memory) -> Result<(), crate::Error> {
        use crate::architecture::arm::core::m4::Demcr;

        // Clear reset catch bit
        let mut demcr = Demcr(core.read_word_32(Demcr::ADDRESS)?);
        demcr.set_vc_corereset(false);

        core.write_word_32(Demcr::ADDRESS, demcr.into())?;
        Ok(())
    }

    fn reset_system(&self, interface: &mut Memory) -> Result<(), crate::Error> {
        use crate::architecture::arm::core::m4::{Aircr, Dhcsr};

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        interface.write_word_32(Aircr::ADDRESS, aircr.into())?;

        let start = Instant::now();

        while start.elapsed() < Duration::from_micros(50_0000) {
            let dhcsr = Dhcsr(interface.read_word_32(Dhcsr::ADDRESS)?);

            // Wait until the S_RESET_ST bit is cleared on a read
            if !dhcsr.s_reset_st() {
                return Ok(());
            }
        }

        Err(crate::Error::Probe(DebugProbeError::Timeout))
    }

    fn debug_device_unlock(&self, _interface: &mut crate::Memory) -> Result<(), crate::Error> {
        // Empty by default
        Ok(())
    }

    fn recover_support_start(&self, _interface: &mut crate::Memory) -> Result<(), crate::Error> {
        // Empty by default
        Ok(())
    }
}
