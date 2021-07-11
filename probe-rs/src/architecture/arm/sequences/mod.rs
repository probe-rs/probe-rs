pub mod nxp;

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use crate::{core::CoreRegister, DebugProbeError, Memory};

use super::{
    communication_interface::{DapProbe, Initialized, SwdSequence},
    dp::{Abort, Ctrl, DpAccess, Select, DPIDR},
    ArmCommunicationInterface, DpAddress, Pins, PortType, Register,
};

pub struct DefaultArmSequence(());

impl DefaultArmSequence {
    pub fn new() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }
}

impl ArmDebugSequence for DefaultArmSequence {}

pub trait ArmDebugSequence: Send + Sync {
    /// Assert a system-wide reset line nRST. This is based on the
    /// `ResetHardwareAssert` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#resetHardwareAssert
    #[doc(alias = "ResetHardwareAssert")]
    fn reset_hardware_assert(&self, interface: &mut dyn DapProbe) -> Result<(), crate::Error> {
        let mut n_reset = Pins(0);
        n_reset.set_nreset(true);

        let _ = interface.swj_pins(0, n_reset.0 as u32, 0)?;

        Ok(())
    }

    /// De-Assert a system-wide reset line nRST. This is based on the
    /// `ResetHardwareDeassert` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#resetHardwareDeassert
    #[doc(alias = "ResetHardwareDeassert")]
    fn reset_hardware_deassert(&self, memory: &mut Memory) -> Result<(), crate::Error> {
        let interface = memory.get_arm_interface()?;

        let mut n_reset = Pins(0);
        n_reset.set_nreset(true);
        let n_reset = n_reset.0 as u32;

        let can_read_pins = interface.swj_pins(n_reset, n_reset, 0)? != 0xffff_ffff;

        if can_read_pins {
            let start = Instant::now();

            while start.elapsed() < Duration::from_secs(1) {
                if Pins(interface.swj_pins(n_reset, n_reset, 0)? as u8).nreset() {
                    return Ok(());
                }
            }

            Err(DebugProbeError::Timeout.into())
        } else {
            thread::sleep(Duration::from_millis(100));
            Ok(())
        }
    }

    /// Prepare the target debug port for connection. This is based on the
    /// `DebugPortSetup` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#debugPortSetup
    #[doc(alias = "DebugPortSetup")]
    fn debug_port_setup(&self, interface: &mut Box<dyn DapProbe>) -> Result<(), crate::Error> {
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
        let _ = interface.raw_read_register(PortType::DebugPort, DPIDR::ADDRESS)?;

        //interface.read_dpidr()?;

        Ok(())
    }

    /// Connect to the target debug port and power it up. This is based on the
    /// `DebugPortStart` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#debugPortStart
    #[doc(alias = "DebugPortStart")]
    fn debug_port_start(
        &self,
        interface: &mut ArmCommunicationInterface<Initialized>,
        dp: DpAddress,
    ) -> Result<(), crate::DebugProbeError> {
        interface.write_dp_register(dp, Select(0))?;

        //let powered_down = interface.read_dp_register::<Select>::()

        let ctrl = interface.read_dp_register::<Ctrl>(dp)?;

        let powered_down = !(ctrl.csyspwrupack() && ctrl.cdbgpwrupack());

        if powered_down {
            let mut ctrl = Ctrl(0);
            ctrl.set_cdbgpwrupreq(true);
            ctrl.set_csyspwrupreq(true);
            interface.write_dp_register(dp, ctrl)?;

            let start = Instant::now();
            let mut timeout = true;
            while start.elapsed() < Duration::from_micros(100_0000) {
                let ctrl = interface.read_dp_register::<Ctrl>(dp)?;
                if ctrl.csyspwrupack() && ctrl.cdbgpwrupack() {
                    timeout = false;
                    break;
                }
            }

            if timeout {
                return Err(DebugProbeError::Timeout);
            }

            // TODO: Handle JTAG Specific part

            // TODO: Only run the following code when the SWD protocol is used

            // Init AP Transfer Mode, Transaction Counter, and Lane Mask (Normal Transfer Mode, Include all Byte Lanes)
            let mut ctrl = Ctrl(0);
            ctrl.set_cdbgpwrupreq(true);
            ctrl.set_csyspwrupreq(true);
            ctrl.set_mask_lane(0b1111);
            interface.write_dp_register(dp, ctrl)?;

            let mut abort = Abort(0);
            abort.set_orunerrclr(true);
            abort.set_wderrclr(true);
            abort.set_stkerrclr(true);
            abort.set_stkcmpclr(true);
            interface.write_dp_register(dp, abort)?;
        }

        Ok(())
    }

    /// Initialize core debug system. This is based on the
    /// `DebugCoreStart` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#debugCoreStart
    #[doc(alias = "DebugCoreStart")]
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

    /// Configure the target to stop code execution after a reset. After this, the core will halt when it comes
    /// out of reset. This is based on the `ResetCatchSet` function from
    /// the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#resetCatchSet
    #[doc(alias = "ResetCatchSet")]
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

    /// Free hardware resources allocated by ResetCatchSet.
    /// This is based on the `ResetCatchSet` function from
    /// the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#resetCatchClear
    #[doc(alias = "ResetCatchClear")]
    fn reset_catch_clear(&self, core: &mut Memory) -> Result<(), crate::Error> {
        use crate::architecture::arm::core::m4::Demcr;

        // Clear reset catch bit
        let mut demcr = Demcr(core.read_word_32(Demcr::ADDRESS)?);
        demcr.set_vc_corereset(false);

        core.write_word_32(Demcr::ADDRESS, demcr.into())?;
        Ok(())
    }

    /// Executes a system-wide reset without debug domain (or warm-reset that preserves debug connection) via software mechanisms,
    /// for example AIRCR.SYSRESETREQ.  This is based on the
    /// `ResetSystem` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#resetSystem
    #[doc(alias = "ResetSystem")]
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

    /// Check if the device is in a locked state and unlock it.
    /// Use query command elements for user confirmation.
    /// Executed after having powered up the debug port. This is based on the
    /// `DebugDeviceUnlock` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#debugDeviceUnlock
    #[doc(alias = "DebugDeviceUnlock")]
    fn debug_device_unlock(&self, _interface: &mut crate::Memory) -> Result<(), crate::Error> {
        // Empty by default
        Ok(())
    }

    /// Executed before step or run command to support recovery from a lost target connection, e.g. after a low power mode.
    /// This is based on the `RecoverSupportStart` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: http://www.keil.com/pack/doc/cmsis/Pack/html/debug_description.html#recoverSupportStart
    #[doc(alias = "RecoverSupportStart")]
    fn recover_support_start(&self, _interface: &mut crate::Memory) -> Result<(), crate::Error> {
        // Empty by default
        Ok(())
    }
}
