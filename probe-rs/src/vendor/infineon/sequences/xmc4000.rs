//! Sequences for XMC4000

use crate::architecture::arm::armv7m::{Aircr, Dhcsr, FpCtrl, FpRev1CompX, FpRev2CompX};
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::{ArmDebugSequence, ArmDebugSequenceError};
use crate::architecture::arm::ArmError;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use crate::architecture::arm::communication_interface::DapProbe;
use crate::{probe::DebugProbeError, MemoryMappedRegister};

/// An Infineon XMC4xxx MCU.
#[derive(Debug)]
pub struct XMC4000 {
    reset_state: Mutex<Option<ResetState>>,
}

impl XMC4000 {
    /// Create the sequencer for an Infineon XMC4000.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self {
            reset_state: Mutex::new(None),
        })
    }
}

#[derive(Debug)]
enum ResetState {
    /// We are commanding a "Warm Reset Halt", per XMC4700/XMC4800 reference manual v1.3 § 28.4.2.
    ///
    /// This variant contains the state we clobbered while setting a breakpoint to halt on the jump
    /// to user code.
    Warm { fpctrl_enabled: bool, fpcomp0: u32 },
    /// We are commanding a "Cold Reset Halt", per XMC4700/XMC4800 reference manual v1.3 § 28.4.2.
    ///
    /// This variant has no state to track.
    Cold { tssw_start_at: Instant },
}
impl Default for ResetState {
    fn default() -> Self {
        ResetState::Warm {
            fpctrl_enabled: false,
            fpcomp0: 0,
        }
    }
}

bitfield::bitfield! {
    /// SCU->STCON startup configuration register, as documented in XMC4700/XMC4800 reference
    /// manual v1.3 § 11-73.
    #[derive(Copy,Clone)]
    pub struct Stcon(u32);
    impl Debug;

    /// > HW Configuration
    /// > At PORESET the following values are latched:
    /// > HWCON.0 = not (TMS)
    /// > HWCON.1 = TCK
    /// >
    /// > 0b00B: Normal mode, JTAG
    /// > 0b01B: ASC BSL enabled
    /// > 0b10B: BMI customized boot enabled
    /// > 0b11B: CAN BSL enabled
    pub hwcon, _: 1, 0;

    /// > SW Configuration
    /// > Bit[9:8] is copy of Bit[1:0] after PORESET
    /// > …
    /// > Note: Only reset with Power-on Reset
    pub swcon, set_swcon: 11, 8;
}
impl Stcon {
    const ADDRESS: u64 = 0x50004010;
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
        core: &mut dyn ArmMemoryInterface,
        core_type: probe_rs_target::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::trace!("performing XMC4000 ResetCatchSet");

        // Did we just come out of a cold reset?
        if let Some(tssw_start_at) = self
            .reset_state
            .lock()
            .map(|v| match v.as_ref() {
                Some(ResetState::Cold { tssw_start_at }) => Some(*tssw_start_at),
                _ => None,
            })
            .unwrap()
        {
            // The core will halt itself after SSW execution _if_ we connected fast enough
            // Attempt to determine how much time passed since the system software started running
            let tssw_elapsed = tssw_start_at.elapsed();
            if tssw_elapsed > Duration::from_micros(2_500) {
                // We might have missed the window
                tracing::warn!(
                    "{:?} has elapsed since power on reset, exceeding typical tSSW of 2.5ms",
                    tssw_elapsed
                );
            }

            // See if we halted
            match spin_until_core_is_halted(core, Duration::from_millis(3)) {
                Err(ArmError::Timeout) => {
                    // We missed the boat
                    tracing::info!("Core did not halt after cold boot; performing a warm reset");

                    // Clear our cold boot status, since we failed
                    self.reset_state.lock().map(|mut s| s.take()).unwrap();

                    // Perform a warm reset
                    self.reset_catch_set(core, core_type, debug_base)?;
                    self.reset_system(core, core_type, debug_base)?;
                }
                Err(e) => return Err(e),
                Ok(()) => {
                    // We halted after cold reset, which is what our caller wanted
                    // Job done
                    return Ok(());
                }
            }
        }

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
        //
        // Read STCON.
        let stcon = Stcon(core.read_word_32(Stcon::ADDRESS)?);

        // The software-settable SWCON field is authoritative for system resets, per § 27.2.3:
        //
        // > HWCON bit field is read only for PORST (Power ON Reset). For every other reset type
        // > (available in SCU_RSTSTAT) register, the SWCON field is assessed.
        //
        // Set it to a normal boot if needed.
        if stcon.swcon() != 0 {
            let mut stcon = stcon;
            stcon.set_swcon(0);
            core.write_word_32(Stcon::ADDRESS, stcon.0)?;
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
        let application_entry = core.read_word_32(0x0C000004)? - 1;

        // Read FP state so we can restore it later
        let fp_ctrl = FpCtrl(core.read_word_32(FpCtrl::get_mmio_address())?);
        let fpcomp0 = core.read_word_32(FpRev1CompX::get_mmio_address())?;

        // Indicate we're in the midst of a warm reset
        self.reset_state
            .lock()
            .map(|mut m| {
                m.replace(ResetState::Warm {
                    fpctrl_enabled: fp_ctrl.enable(),
                    fpcomp0,
                })
            })
            .unwrap();

        // Enable FP
        let mut fp_ctrl = FpCtrl(0);
        fp_ctrl.set_enable(true);
        fp_ctrl.set_key(true);
        core.write_word_32(FpCtrl::get_mmio_address(), fp_ctrl.into())?;

        // Set a breakpoint at application_entry
        let val = if fp_ctrl.rev() == 0 {
            FpRev1CompX::breakpoint_configuration(application_entry)?.into()
        } else if fp_ctrl.rev() == 1 {
            FpRev2CompX::breakpoint_configuration(application_entry).into()
        } else {
            return Err(ArmDebugSequenceError::custom(format!(
                "xmc4000: unexpected fp_ctrl.rev = {}",
                fp_ctrl.rev()
            ))
            .into());
        };
        core.write_word_32(FpRev1CompX::get_mmio_address(), val)?;
        tracing::debug!("Set a breakpoint at {:08x}", application_entry);

        core.flush()?;

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::trace!("performing XMC4000 ResetCatchClear");

        // Grab the prior breakpoint state
        let reset_state = self
            .reset_state
            .lock()
            .map(|mut m| m.take().unwrap_or_default())
            .unwrap();

        match reset_state {
            ResetState::Warm {
                fpctrl_enabled,
                fpcomp0,
            } => {
                // Put FPCTRL back
                let mut fpctrl = FpCtrl::from(0);
                fpctrl.set_key(true);
                fpctrl.set_enable(fpctrl_enabled);
                core.write_word_32(FpCtrl::get_mmio_address(), fpctrl.into())?;

                // Put FPCOMP0 back
                core.write_word_32(FpRev1CompX::get_mmio_address(), fpcomp0)?;
            }
            ResetState::Cold { .. } => {
                // No op
            }
        }

        Ok(())
    }

    fn reset_system(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
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
        core.write_word_32(Aircr::get_mmio_address(), aircr.into())?;
        tracing::debug!("Resetting via AIRCR.SYSRESETREQ");

        // Spin until CoreSight indicates the reset was processed
        let start = Instant::now();
        loop {
            let dhcsr = Dhcsr(core.read_word_32(Dhcsr::get_mmio_address())?);

            // Wait until the S_RESET_ST bit is cleared on a read
            if !dhcsr.s_reset_st() {
                tracing::debug!("Detected reset via S_RESET_ST");
                break;
            }
            if start.elapsed() > Duration::from_millis(500) {
                tracing::error!("XMC4000 did not reset as commanded");
                return Err(ArmError::Timeout);
            }
        }

        // Now that we commanded a reset, we've lost access to everything but CoreSight.
        // XMC4700/XMC4800 reference manual v1.3 § 28-7:
        //
        // > For security reasons it is required to prevent a debug access to the processor before
        // > and while the boot firmware code from ROM (SSW) is being executed. A bit DAPSA, (DAP
        // > has system access) in the SCU is implemented, allowing the access from CoreSight™ debug
        // > system to the processor core. The default value of this bit is disabled debug access.
        // > The register is reset by System Reset. The System Reset disables the debug access each
        // > time SSW is being executed. At the end of the SSW the DAPSA is enabled always
        // > (independent of any other register setting or signaling value), to allow debug access
        // > to the CPU. A tool accessing the SoC during the SSW execution time reads back a zero
        // > and a write is going to a virtual, none existing address.
        //
        spin_until_dapsa_is_clear(core)?;

        // If we are intending to halt after reset, spin and wait for that here
        if self.reset_state.lock().map(|v| v.is_some()).unwrap() {
            tracing::debug!("Waiting for XMC4000 to halt after reset");
            // We're doing a halt-after-reset
            // Wait for the core to halt
            spin_until_core_is_halted(core, Duration::from_millis(1000))?;
        } else {
            tracing::debug!("not performing a halt-after-reset");
        }

        Ok(())
    }

    fn reset_hardware_assert(&self, interface: &mut dyn DapProbe) -> Result<(), ArmError> {
        tracing::trace!("performing XMC4000 ResetHardwareAssert");

        use crate::architecture::arm::Pins;

        // We want to drive nRST, TCK, and TMS
        let mut pin_select = Pins(0);
        pin_select.set_nreset(true);
        pin_select.set_swclk_tck(true);
        pin_select.set_swdio_tms(true);

        // We want to drive nRST low to command the reset
        let mut pin_output = Pins(0);
        pin_output.set_nreset(false);
        // HWCON is latched at power-on reset to be [TCK, !TMS], and we want HWCON to be zero, so
        // we want to drive TCK low and TMS high.
        pin_output.set_swclk_tck(false);
        pin_output.set_swdio_tms(true);

        loop {
            match interface.swj_pins(pin_output.0 as u32, pin_select.0 as u32, 0) {
                Err(DebugProbeError::CommandNotSupportedByProbe {
                    command_name: "swj_pins",
                }) if pin_select.swdio_tms() => {
                    // J-Link probes return this error when we try to set pins besides nRST
                    // Settle for resetting, but warn the user that HWCON is uncontrolled
                    tracing::debug!("swj_pins(nRST|TCK|TMS) unsupported by probe; falling back to swj_pins(nRST)");
                    tracing::warn!(
                        "This probe cannot manipulate HWCON, so the boot mode after power on reset cannot be controlled"
                    );
                    pin_select = Pins(0);
                    pin_select.set_nreset(true);
                }
                Err(other) => return Err(other.into()),
                Ok(_) => break,
            }
        }

        // When we return, the caller will to attach the probe and set DEBUGEN. XMC4000 needs that
        // to happen while the SSW is booting, _after_ releasing nRST. This means we need to
        // deassert nRST here rather than waiting for ResetHardwareDeassert.
        //
        // Wait a moment for the reset signal to settle.
        thread::sleep(Duration::from_millis(100));

        // Indicate to ourselves that we're doing a cold reset, and that the system software began
        // executing now
        self.reset_state
            .lock()
            .map(|mut s| {
                s.replace(ResetState::Cold {
                    tssw_start_at: Instant::now(),
                })
            })
            .unwrap();

        // Deassert nRST
        pin_output.set_nreset(true);
        interface.swj_pins(pin_output.0 as u32, pin_select.0 as u32, 0)?;

        // Race! :(

        Ok(())
    }

    fn reset_hardware_deassert(&self, memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        tracing::trace!("performing XMC4000 ResetHardwareDeassert");

        // We already deasserted nRST in ResetHardwareAssert, because that's how Cold Reset Halts
        // work on this platform.

        // We should however wait until the SSW is ready.
        spin_until_dapsa_is_clear(memory)?;

        Ok(())
    }
}

fn spin_until_core_is_halted(
    core: &mut dyn ArmMemoryInterface,
    timeout: Duration,
) -> Result<(), ArmError> {
    let start = Instant::now();
    loop {
        let dhcsr = Dhcsr(core.read_word_32(Dhcsr::get_mmio_address())?);
        if dhcsr.s_halt() {
            tracing::debug!("Halted after reset");
            return Ok(());
        } else if start.elapsed() > timeout {
            tracing::error!("XMC4000 did not halt after reset");
            return Err(ArmError::Timeout);
        }
    }
}

fn spin_until_dapsa_is_clear(core: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    let start = Instant::now();
    loop {
        // DAPSA isn't directly accessible because of course it isn't.
        //
        // Read the SCU module ID register, which is guaranteed to be nonzero. If DAPSA is set,
        // we'll read it normally, and we can go on with our lives. If DAPSA is clear, we'll
        // read a zero.
        let scu_module_id = core.read_word_32(0x5000_4000)?;
        if scu_module_id != 0 {
            tracing::debug!("DAPSA is set");
            break Ok(());
        } else {
            tracing::trace!("DAPSA is clear");
            if start.elapsed() > Duration::from_millis(500) {
                tracing::error!("timed out waiting for DAPSA to clear, indicating SSW hang");
                break Err(ArmError::Timeout);
            }
        }
    }
}
