//! Sequences for Infineon target families

use crate::architecture::arm::armv7m::{Aircr, Dhcsr, FpCtrl, FpRev1CompX, FpRev2CompX};
use anyhow::anyhow;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::Error;
use crate::Memory;
use crate::{DebugProbeError, MemoryMappedRegister};

use super::ArmDebugSequence;

/// An Infineon XMC4xxx MCU.
pub struct XMC4000 {
    halt_after_reset_state: Mutex<Option<(bool, u32)>>,
}

impl XMC4000 {
    /// Create the sequencer for an Infineon XMC4000.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self {
            halt_after_reset_state: Mutex::new(None),
        })
    }
}

impl ArmDebugSequence for XMC4000 {
    // We have a weird halt-after-reset sequence. It's described only in prose, not in a CMSIS pack
    // sequence. Per XMC4700/XMC4800 reference manual v1.3 § 28-8:
    //
    // > A Halt after system reset (Warm Reset) can be achieved by programming a break point at the
    // > first instruction of the application code. Before a Warm Reset the CTRL/STAT.CDBGPWRUPREQ
    // > and the DHCSR.C_DEBUGEN setting has to be ensured. After a system reset, the HAR situation
    // > is not considered, as the reset is not coming from "Power-on Reset".
    // >
    // > Note: The CTRL/STAT.CDBGPWRUPREQ and DHCSR.C_DEBUGEN does not have to be set after a
    // > system reset, if they have already been set before.
    // >
    // > A tool hot plug condition allows to debug the system starting with a tool registration
    // > (setting CTRL/STAT.CDBGPWRUPREQ register) and a debug system enable (setting the
    // > DHCSR.C_DEBUGEN register). Afterwards break points can be set or the CPU can directly by
    // > HALTED by an enable of C_HALT.
    //
    // So:
    // * ResetCatchSet must determine the first user instruction and set a breakpoint there.
    // * ResetCatchClear must restore the clobbered breakpoint, if any.

    fn reset_catch_set(
        &self,
        core: &mut Memory,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), Error> {
        tracing::trace!("performing XMC4000 ResetCatchSet");

        // We need to find the "first instruction of the application code". The XMC4xxx system
        // software ("SSW") has a vector table at the start of boot ROM. XMC4700/XMC4800 reference
        // manual v1.3 § 2-31:
        //
        // > On system reset, the vector table is fixed at address 0x00000000.
        //
        // That reset vector runs first, but it doesn't qualify as "application code". § 27.2:
        //
        // > This section describes the startup sequence of the XMC4[78]00 as a process taking place
        // > before user application software takes control of the system.
        //
        // So: where does the application begin? That depends on the boot mode! The boot mode
        // depends on the System Control Unit (SCU) Startup Control (STCON) register, which includes
        // both a software-settable field and a power-on-resettable field. We want a normal boot,
        // because normal boots are sane and reasonable.
        let stcon = core.read_word_32(0x50004010)?;
        if stcon & 0x3 != 0 {
            // HWCON is nonzero, preventing a normal boot, and HWCON is latched at power-on reset
            //
            // In principle we can fix this because a) HWCON is connected to the debugger pins and
            // b) the DEBUG_CM.002 erratum lets us perform power-on resets without disrupting
            // debugger state (!), but... that's complicated.
            //
            // Settle for detecting this condition and producing an error.
            tracing::error!(
                "normal boots are impossible without a power on reset: SCU STCON = {:08x}",
                stcon
            );
            return Err(Error::Other(anyhow!(
                "SCU STCON HWCON must be zero to perform a normal boot"
            )));
        }
        if stcon != 0 {
            // Ask for a normal boot
            core.write_word_32(0x50004010, 0)?;
        }

        // § 27.3.1 describes the normal boot mode, which happens after firmware initialization:
        //
        // > Firmware essentially reprograms the Cortex M4’s SCB.VTOR register with the start
        // > address of flash (0C000000H) and passes control to user application by programing
        // > register R15 (Program Counter) with reset vector contents. The reset vector contents
        // > point to a routine that could be in either the cached or the uncached address space of
        // > the flash.
        //
        // Therefore, "the first instruction of the application code" for normal boot mode is the
        // instruction pointed to by the reset vector at the start of flash, i.e. 0x0C000004.
        //
        // This is also why we have to use a breakpoint instead of trapping the reset vector. The
        // application's entrypoint is a normal jump from the firmware, not an exception dispatched
        // to the reset vector.
        let application_entry = core.read_word_32(0x0C000004)? + 1;

        // Read FPCTRL
        let fp_ctrl = FpCtrl(core.read_word_32(FpCtrl::ADDRESS)?);
        let enabled = fp_ctrl.enable();

        // Read FPCOMP0 so we can restore it later
        let value = core.read_word_32(FpRev1CompX::ADDRESS)?;
        self.halt_after_reset_state
            .lock()
            .map(|mut m| m.replace((enabled, value)))
            .unwrap();

        // Enable FP
        let mut fp_ctrl = FpCtrl(0);
        fp_ctrl.set_enable(true);
        fp_ctrl.set_key(true);
        core.write_word_32(FpCtrl::ADDRESS, fp_ctrl.into())?;

        // Set a breakpoint at application_entry
        let val = if fp_ctrl.rev() == 0 {
            FpRev1CompX::breakpoint_configuration(application_entry)?.into()
        } else if fp_ctrl.rev() == 1 {
            FpRev2CompX::breakpoint_configuration(application_entry).into()
        } else {
            return Err(Error::Other(anyhow!(
                "xmc4000: unexpected fp_ctrl.rev = {}",
                fp_ctrl.rev()
            )));
        };
        core.write_word_32(FpRev1CompX::ADDRESS, val)?;
        tracing::debug!("Set a breakpoint at {:08x}", application_entry);

        core.flush()?;

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        core: &mut Memory,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), Error> {
        tracing::trace!("performing XMC4000 ResetCatchClear");

        // Grab the prior breakpoint state
        let (enabled, previous_breakpoint) = self
            .halt_after_reset_state
            .lock()
            .map(|mut m| m.take().unwrap_or((false, 0)))
            .unwrap();

        // Put FPCTRL back
        let mut fpctrl = FpCtrl::from(0);
        fpctrl.set_key(true);
        fpctrl.set_enable(enabled);
        core.write_word_32(FpCtrl::ADDRESS, fpctrl.into())?;

        // Put FPCOMP0 back
        core.write_word_32(FpRev1CompX::ADDRESS, previous_breakpoint)?;

        Ok(())
    }

    fn reset_system(
        &self,
        core: &mut Memory,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), Error> {
        // XMC4700/XMC4800 reference manual v1.3 § 27.2.2.2:
        // > Since the Reset Status Information in register SCU.RSTSTAT is the accumulated reset
        // > type, it is necessary to clean the bitfield using the SCU register RSTCLR.RSCLR before
        // > issuing a System Reset, else SSW will enter the boot mode reflected in STCON.HWCON.
        //
        // ARM KB https://developer.arm.com/documentation/ka002435:
        // > After the user program has handled the the reset sources itself, it needs to clear them
        // > by executing:
        // >
        // >    SCU_RESET->RSTCLR = SCU_RESET_RSTCLR_RSCLR_Msk ;
        // >
        // > before the next reset. This will clear also the PORST bit. And if the next reset is no
        // > Power On Reset, the SSW will also not do HAR.
        //
        // This is normally handled by the runtime, but let's be defensive.
        //
        // SCU_RESET->RSTCLR is at 0x5000_4408, and RSCLR is the low bit.
        core.write_word_32(0x5000_4408, 1)?;
        tracing::debug!("Cleared SCU_RESET->RSTSTAT");

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        core.write_word_32(Aircr::ADDRESS, aircr.into())?;
        tracing::debug!("Resetting via AIRCR.SYSRESETREQ");

        // Do not drive the debug pins for a short while
        // We do not want to clobber the boot mode
        std::thread::sleep(Duration::from_millis(100));

        let start = Instant::now();
        loop {
            let dhcsr = Dhcsr(core.read_word_32(Dhcsr::ADDRESS)?);

            // Wait until the S_RESET_ST bit is cleared on a read
            if !dhcsr.s_reset_st() {
                tracing::debug!("Detected reset via S_RESET_ST");
                break;
            } else if start.elapsed() > Duration::from_millis(500) {
                tracing::error!("XMC4000 did not reset as commanded");
                return Err(crate::Error::Probe(DebugProbeError::Timeout));
            }
        }

        if self
            .halt_after_reset_state
            .lock()
            .map(|v| v.is_some())
            .unwrap()
        {
            tracing::debug!("Waiting for XMC4000 to halt after reset");
            // We're doing a halt-after-reset
            // Wait for the core to halt
            loop {
                let dhcsr = Dhcsr(core.read_word_32(Dhcsr::ADDRESS)?);
                if dhcsr.s_halt() {
                    tracing::debug!("Halted after reset");
                    break;
                } else if start.elapsed() > Duration::from_millis(1000) {
                    tracing::error!("XMC4000 did not halt after reset");
                    return Err(crate::Error::Probe(DebugProbeError::Timeout));
                }
            }
        } else {
            tracing::debug!("not performing a halt-after-reset");
        }

        Ok(())
    }
}
