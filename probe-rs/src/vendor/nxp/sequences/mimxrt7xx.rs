//! Sequences for NXP i.MX RT7xx (MIMXRT798S) MCUs.
//!
//! Derived from the debug description NXP ships for the family, cross-checked against the
//! Armv8-M Architecture Reference Manual and against what the silicon actually does.

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bitfield::bitfield;
use probe_rs_target::CoreType;

use crate::{
    MemoryMappedRegister, RegisterId,
    architecture::arm::{
        ApAddress, ArmDebugInterface, ArmError, DapAccess, FullyQualifiedApAddress,
        ap::{ApRegister, CSW, IDR},
        core::{
            armv8m::{Aircr, Demcr, Dhcsr},
            cortex_m,
            registers::cortex_m::XPSR,
        },
        dp::{Abort, Ctrl, DpAccess, DpAddress, SelectV1},
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, DefaultArmSequence},
    },
};

bitfield! {
    /// DWT Comparator Function register, `DWT_FUNCTION`.
    ///
    /// This is the Armv8-M layout, which is not the one
    /// [`component::dwt::Function`](crate::architecture::arm::component) models: there the
    /// low bits are the Armv7-M `FUNCTION`/`EMITRANGE` fields, whereas v8-M splits them
    /// into `MATCH` and `ACTION`.
    ///
    /// Reference: `D1.2.38` in `Armv8-M Architecture Reference Manual`.
    #[derive(Copy, Clone)]
    pub struct DwtFunction(u32);
    impl Debug;
    /// Size of the data value the comparator matches against: 0b10 is a word.
    pub u8, _, set_datavsize: 11, 10;
    /// What happens on a match: 0b01 generates a debug event, halting the core.
    pub u8, _, set_action: 5, 4;
    /// What the comparator matches on.
    pub u8, _, set_match: 3, 0;
}

/// Debug sequences for the i.MX RT7xx family.
///
/// These parts have no internal flash, and they are picky about how debug power is
/// requested. The stock
/// [`debug_port_start_default`](ArmDebugSequence::debug_port_start_default) writes
/// `CDBGPWRUPREQ`/`CSYSPWRUPREQ` *together with* `MASKLANE` in a single `CTRL/STAT`
/// write (`0x50000F00`). An RT798S then acknowledges `CSYSPWRUPREQ` but never
/// `CDBGPWRUPREQ` -- `CTRL/STAT` sits at `0xD0000040` indefinitely -- and the connection
/// fails with a timeout.
///
/// Requesting power-up on its own (`0x50000000`), waiting for both acknowledgements, and
/// only then adding `MASKLANE` gets an acknowledgement in a few hundred microseconds.
/// That ordering is the whole reason this sequence exists.
///
/// Debug access can also be gated by the boot ROM, on a locked part or one sitting in ISP
/// mode. The recovery for that is the debug mailbox on AP 2, which lives in the always-on
/// power domain and so stays reachable while debug power is down; asking it for a debug
/// session makes the ROM hand access over. On a stock MIMXRT798S-EVK the power-up above
/// already succeeds and the mailbox is never reached.
///
/// `DebugPortStart`, `DebugCoreStart`/`on_attach` and `ResetSystem` are implemented.
#[derive(Debug)]
#[non_exhaustive]
pub struct MIMXRT7xx;

impl MIMXRT7xx {
    /// The AP hosting the NXP debug mailbox.
    const DEBUG_MAILBOX_AP: u8 = 2;

    /// AP of the primary (compute) core, `cm33_core0`.
    const CORE0_AP: u8 = 0;

    /// AP of the secondary (sense) core, `cm33_core1`.
    const CORE1_AP: u8 = 1;

    /// Debug mailbox register offsets.
    const DM_CSW: u64 = 0x00;
    const DM_REQUEST: u64 = 0x04;
    const DM_RETURN: u64 = 0x08;

    /// `RESYNCH_REQ | CHIP_RESET_REQ`
    const DM_RESYNC_REQ: u32 = 0x0000_0021;
    /// `START_DBG_SESSION`
    const DM_START_DEBUG_SESSION: u32 = 0x0000_0007;

    /// DWT registers, used to catch the boot ROM before it leaves for the application.
    const DWT_COMP0: u64 = 0xE000_1020;
    const DWT_FUNCTION0: u64 = 0xE000_1028;

    /// Address the boot ROM reads once it is nearly done starting up. Catching that read
    /// is how we stop execution at the end of the ROM rather than at its entry. Note the
    /// address differs from the RT5xx/RT6xx one (`0x5000_2034`).
    const SYSTEM_STICK_CALIB_ADDR: u32 = 0x5000_2094;

    /// `DCRSR.REGSEL` for the secure main stack pointer limit, per probe-rs' own register
    /// table: `MSPLIM_S` is `RegisterId(0b0001_1100)`.
    const REGSEL_MSPLIM_S: u16 = 0x1C;

    /// How long the boot ROM needs after a system reset before debug access can be
    /// re-established.
    const BOOT_TIME: Duration = Duration::from_millis(50);

    /// How long the ROM is given to act on a mailbox command.
    const DM_COMMAND_TIMEOUT: Duration = Duration::from_millis(100);

    /// How long to wait for the power-up request to be acknowledged.
    const POWER_UP_TIMEOUT: Duration = Duration::from_secs(1);

    /// Create a sequence handle for the i.MX RT7xx.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self)
    }

    /// The `ABORT` bits that clear the sticky error flags, optionally also aborting a
    /// stalled transfer.
    fn abort(abort_transfer: bool) -> Abort {
        let mut abort = Abort(0);
        abort.set_orunerrclr(true);
        abort.set_wderrclr(true);
        abort.set_stkerrclr(true);
        abort.set_stkcmpclr(true);
        abort.set_dapabort(abort_transfer);
        abort
    }

    /// Request debug and system power-up, and report whether both requests were
    /// acknowledged within [`MIMXRT7xx::POWER_UP_TIMEOUT`].
    ///
    /// Crucially this writes *only* the two request bits: adding `MASKLANE` here is what
    /// stops the part from ever acknowledging. The caller sets the lane mask afterwards.
    ///
    /// A missing `CDBGPWRUPACK` is not an error. It can simply mean the boot ROM has not
    /// granted debug access yet, which the caller recovers from via the debug mailbox.
    fn request_power_up(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<bool, ArmError> {
        let mut ctrl = Ctrl(0);
        ctrl.set_csyspwrupreq(true);
        ctrl.set_cdbgpwrupreq(true);
        interface.write_dp_register(dp, ctrl)?;

        let start = Instant::now();
        loop {
            let ctrl = interface.read_dp_register::<Ctrl>(dp)?;
            if ctrl.csyspwrupack() && ctrl.cdbgpwrupack() {
                tracing::debug!("RT7xx debug port powered up after {:?}", start.elapsed());
                return Ok(true);
            }
            if start.elapsed() >= Self::POWER_UP_TIMEOUT {
                tracing::debug!(
                    "RT7xx power-up was not acknowledged within {:?} (CTRL/STAT: {:#010x})",
                    Self::POWER_UP_TIMEOUT,
                    ctrl.0
                );
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Only safe once power-up has been acknowledged: setting the lane mask alongside the
    /// power-up request is what stops the part from acknowledging in the first place.
    fn init_ap_transfer_mode(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        let mut ctrl = Ctrl(0);
        ctrl.set_csyspwrupreq(true);
        ctrl.set_cdbgpwrupreq(true);
        ctrl.set_mask_lane(0b1111);
        interface.write_dp_register(dp, ctrl)?;

        interface.write_dp_register(dp, Self::abort(false))
    }

    /// Whether the given memory AP reports `CSW.DeviceEn`, i.e. debug access has been
    /// granted and the AP is usable for memory transactions.
    ///
    /// A read that does not complete is reported as an error, *not* as "not enabled".
    /// The two mean very different things: an AP the ROM has not enabled answers with
    /// `DeviceEn` clear, whereas a failed read means the DP itself is unhappy. Callers
    /// that recover by asking the debug mailbox for a session must not conflate them,
    /// because that request carries `CHIP_RESET_REQ` and resets the part.
    fn is_ap_enabled(
        &self,
        interface: &mut dyn DapAccess,
        ap: &FullyQualifiedApAddress,
    ) -> Result<bool, ArmError> {
        let csw: CSW = interface
            .read_raw_ap_register(ap, CSW::ADDRESS)?
            .try_into()?;

        Ok(csw.DeviceEn())
    }

    /// Enable debug on `cm33_core1` and report whether it actually took effect, i.e.
    /// `DHCSR.C_DEBUGEN` reads back as set.
    ///
    /// For a core whose power domain is off the write is swallowed and DHCSR reads back
    /// as zero, which is what distinguishes a real core from an AP with nothing behind
    /// it. This is the readback that `cortex_m_core_start` omits.
    fn enable_core1_debug(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
    ) -> bool {
        let mut memory = match interface.memory_interface(core_ap) {
            Ok(memory) => memory,
            Err(error) => {
                tracing::debug!("could not reach {core_ap:?}: {error}");
                return false;
            }
        };

        let mut request = Dhcsr(0);
        request.set_c_debugen(true);
        request.enable_write();
        if let Err(error) = memory.write_word_32(Dhcsr::get_mmio_address(), request.into()) {
            tracing::debug!("could not enable debug on {core_ap:?}: {error}");
            return false;
        }

        match memory.read_word_32(Dhcsr::get_mmio_address()) {
            Ok(dhcsr) => {
                let dhcsr = Dhcsr(dhcsr);
                tracing::debug!("{core_ap:?} DHCSR after enabling debug: {:#010x}", dhcsr.0);
                dhcsr.c_debugen()
            }
            Err(error) => {
                tracing::debug!("could not read DHCSR of {core_ap:?}: {error}");
                false
            }
        }
    }

    /// Release the sense core from reset: unlock the compute domain's access through
    /// GLIKEY and the arbiter, enable its clock, and drop its reset.
    ///
    /// All writes go through AP 0, because the core behind AP 1 does not respond until
    /// this has run. The landing zone matters: the core is brought up on a vector table
    /// whose reset handler is an endless loop, so it comes out of reset parked and
    /// halt-able instead of running whatever happens to be in SRAM.
    fn enable_cpu1(
        &self,
        interface: &mut dyn ArmDebugInterface,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        tracing::info!("booting RT7xx cm33_core1");

        let core0_ap = FullyQualifiedApAddress::v1_with_dp(dp, Self::CORE0_AP);
        let mut core0 = interface.memory_interface(&core0_ap)?;

        for (address, value) in [
            // GLIKEY1: unlock index 9
            (0x4022_0c00u64, 0x0006_0000u32), // clear bit 18
            (0x4022_0c00, 0x0002_0009),
            (0x4022_0c00, 0x0001_0009),
            (0x4022_0c04, 0x0029_0000),
            (0x4022_0c00, 0x0002_0009),
            (0x4022_0c04, 0x0028_0000),
            (0x4022_0c00, 0x0000_0009),
            // arbiter1: let the compute domain through
            (0x4022_0f80, 0x3fff_ffff),
            (0x4022_0f84, 0x3fff_ffff),
            // SLEEPCON0->RUNCFG_CLR[1:] = 0
            (0x4000_3030, 0x0000_0002),
            // Landing zone
            (0x0058_0000, 0x005c_0000), // initial SP
            (0x0058_0004, 0x0058_0009), // reset vector -> 0x580008, thumb
            (0x0058_0008, 0xe7fe_e7fe), // b .
            // GLIKEY4: clear config
            (0x4006_2c00, 0x0006_0000),
            // CLKCTL3->PSCCTL0_COMP_SET = 1
            (0x4006_1040, 0x0000_0001),
            // RSTCTL3->PRSTCTL0_CLR: release the sense core's reset
            (0x4006_0070, 0x8000_0000),
            // SYSCON3->CPU_STATUS = 0
            (0x4006_208c, 0x0000_0000),
        ] {
            core0.write_word_32(address, value)?;
        }
        core0.flush()?;

        tracing::info!("RT7xx cm33_core1 booted");

        Ok(())
    }

    /// Re-establish debug access after the boot ROM has run, halt the core and undo the
    /// reset catch watchpoint.
    fn wait_for_stop_after_reset(&self, core: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        let ap = core.fully_qualified_address();
        let dp = ap.dp();

        // The reset faulted whichever transaction was in flight, so clear the sticky
        // error before using the DP again.
        core.get_arm_debug_interface()?
            .write_dp_register(dp, Self::abort(false))?;

        // Right after a system reset the ROM may not have re-enabled debug access, so a
        // read that faults here means the same thing as `DeviceEn` being clear: ask the
        // mailbox for a session. This is the one place where folding the two together is
        // correct.
        let ap_enabled = self
            .is_ap_enabled(core.get_arm_debug_interface()?, &ap)
            .unwrap_or(false);

        if !ap_enabled {
            tracing::debug!("debug access was not restored after reset, trying the mailbox");
            self.enable_debug_mailbox(core.get_arm_debug_interface()?, dp)?;
        }

        // Halt the core, in case it did not stop at the watchpoint.
        let mut dhcsr = Dhcsr(0);
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);
        dhcsr.enable_write();
        core.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

        core.write_word_32(Self::DWT_COMP0, 0)?;
        core.write_word_32(Self::DWT_FUNCTION0, 0)?;

        // We halted part-way through ROM code. Clear XPSR so a partially executed
        // IT/ICI block cannot fault the first instruction we resume on, and drop MSPLIM
        // so the stack pointer the application installs is accepted.
        //
        // `write_core_reg` drives DCRDR/DCRSR and waits for `DHCSR.S_REGRDY`, which bare
        // register writes would skip.
        cortex_m::write_core_reg(core, XPSR.id, 0x0100_0000)?;
        cortex_m::write_core_reg(core, RegisterId(Self::REGSEL_MSPLIM_S), 0)?;
        core.flush()?;

        tracing::trace!("RT7xx halted after reset");

        Ok(())
    }

    /// Ask the boot ROM for a debug session over the mailbox on AP 2.
    ///
    /// Most of these transactions can fault while the chip resets, so their results are
    /// deliberately not propagated; whether the session came up is decided by the caller
    /// re-checking the DP and AP state afterwards.
    fn enable_debug_mailbox(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        tracing::info!("requesting an RT7xx debug session over the debug mailbox");

        let dm_ap = FullyQualifiedApAddress::v1_with_dp(dp, Self::DEBUG_MAILBOX_AP);

        // Probing AP 0 will have faulted, leaving a stalled transfer and a sticky error
        // behind. Clear both so the mailbox accesses start from a clean DP.
        interface.write_dp_register(dp, Self::abort(true))?;

        // Purely a diagnostic: a sane value here is a good sign that AP 2 really is the
        // mailbox on this part.
        match interface.read_raw_ap_register(&dm_ap, IDR::ADDRESS) {
            Ok(apidr) => match IDR::try_from(apidr) {
                Ok(apidr) => tracing::debug!("RT7xx debug mailbox APIDR: {apidr:?}"),
                Err(_) => tracing::debug!("RT7xx debug mailbox APIDR: {apidr:#010x}"),
            },
            Err(error) => tracing::debug!("RT7xx debug mailbox APIDR unreadable: {error}"),
        }

        interface.write_raw_ap_register(&dm_ap, Self::DM_CSW, Self::DM_RESYNC_REQ)?;
        interface.flush()?;
        self.poll_mailbox(interface, &dm_ap, Self::DM_CSW, "RESYNC_REQ");

        interface.write_raw_ap_register(&dm_ap, Self::DM_REQUEST, Self::DM_START_DEBUG_SESSION)?;
        interface.flush()?;
        self.poll_mailbox(interface, &dm_ap, Self::DM_RETURN, "START_DBG_SESSION");

        Ok(())
    }

    /// Wait for a mailbox status register to report success (low half-word zero).
    ///
    /// Failure is only logged, for the reason given on [`MIMXRT7xx::enable_debug_mailbox`].
    fn poll_mailbox(
        &self,
        interface: &mut dyn DapAccess,
        dm_ap: &FullyQualifiedApAddress,
        register: u64,
        command: &str,
    ) {
        let start = Instant::now();
        loop {
            let timed_out = start.elapsed() >= Self::DM_COMMAND_TIMEOUT;
            match interface.read_raw_ap_register(dm_ap, register) {
                Ok(value) if value & 0xFFFF == 0 => {
                    tracing::debug!(
                        "RT7xx debug mailbox {command}: success after {:?}",
                        start.elapsed()
                    );
                    return;
                }
                Ok(value) if timed_out => {
                    tracing::warn!("RT7xx debug mailbox {command} returned {value:#010x}");
                    return;
                }
                Err(error) if timed_out => {
                    tracing::debug!("RT7xx debug mailbox {command} unreadable: {error}");
                    return;
                }
                _ => thread::sleep(Duration::from_millis(5)),
            }
        }
    }
}

impl ArmDebugSequence for MIMXRT7xx {
    fn debug_port_start(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        tracing::trace!("RT7xx debug port start");

        interface.write_dp_register(dp, SelectV1(0))?;
        interface.write_dp_register(dp, Self::abort(false))?;

        let ctrl = interface.read_dp_register::<Ctrl>(dp)?;
        let mut powered = ctrl.csyspwrupack() && ctrl.cdbgpwrupack();

        if !powered {
            tracing::trace!("RT7xx debug port is powered down, requesting power-up");
            powered = self.request_power_up(interface, dp)?;
        } else {
            tracing::trace!("RT7xx debug port is already powered");
        }

        // Only once the handshake has landed. While it is still unresolved the lane mask
        // would go out as `CTRL/STAT = 0x50000F00` -- the exact write this part refuses
        // to acknowledge a power-up request for -- and there is nothing to gain from
        // sending it before the retry below rewrites the register anyway.
        if powered {
            self.init_ap_transfer_mode(interface, dp)?;
        }

        // If debug power was refused, or was granted but AP 0 is still disabled, the ROM
        // has not handed over debug access yet. Asking the mailbox for a session is what
        // makes it do so.
        //
        // The short circuit is load-bearing: when power-up was refused, AP 0 is expected
        // to be unreadable and we go straight to the mailbox. Only when power is up do we
        // read `CSW`, and then a failed read propagates rather than being treated as
        // "disabled" -- the mailbox request would reset the part, which is far too much
        // to do on the strength of one bus error.
        let core0_ap = FullyQualifiedApAddress::v1_with_dp(dp, Self::CORE0_AP);
        if !powered || !self.is_ap_enabled(interface, &core0_ap)? {
            self.enable_debug_mailbox(interface, dp)?;

            // That request carried `CHIP_RESET_REQ`, so everything learned about the
            // power state above is now stale -- the part may well have come back with
            // the debug domain off. Re-establish it unconditionally, and this time a
            // missing acknowledgement is fatal: we are out of ways to recover.
            if !self.request_power_up(interface, dp)? {
                tracing::error!(
                    "RT7xx did not power up the debug port, even after requesting a \
                     debug session through the debug mailbox"
                );
                return Err(ArmError::Timeout);
            }

            self.init_ap_transfer_mode(interface, dp)?;
        }

        tracing::trace!("RT7xx debug port start was successful");

        Ok(())
    }

    fn debug_core_start(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        core_type: CoreType,
        debug_base: Option<u64>,
        cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        if core_ap.ap() == &ApAddress::V1(Self::CORE1_AP) {
            // Bringing up `cm33_core1` is deferred to `on_attach`. Reporting a problem
            // from here would abort session creation outright --
            // `Session::attach_arm_debug_interface` propagates this for every declared
            // core -- and take core 0 down with it. `on_attach` runs per core access, so
            // it can report just this core as disabled and leave core 0 working.
            return Ok(());
        }

        DefaultArmSequence(()).debug_core_start(interface, core_ap, core_type, debug_base, cti_base)
    }

    fn on_attach(
        &self,
        interface: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
        core_type: CoreType,
        target: &mut crate::Target,
    ) -> Result<(), ArmError> {
        if ap.ap() != &ApAddress::V1(Self::CORE1_AP) {
            return DefaultArmSequence(()).on_attach(interface, ap, core_type, target);
        }

        // `cm33_core1` lives in the sense power domain, which the boot ROM leaves held in
        // reset unless the application releases it. Its AP answers either way -- it
        // reports a perfectly normal MEM-AP IDR -- but the core's debug block behind it
        // is inert: `DHCSR.C_DEBUGEN` does not stick and reads back as zero. Since
        // `cortex_m_core_start` writes C_DEBUGEN without reading it back, left alone this
        // only surfaces later as a halt timeout that fails the whole operation.
        //
        // Releasing the core runs at most once per session: afterwards the check below
        // passes straight away, and if the application started the sense domain itself we
        // never touch it.
        //
        // `on_attach` does fire for every core on every `Session::core()` access, though,
        // including the sweeps `clear_all_hw_breakpoints` and `halted_access` do. So a
        // core-0-only flash also parks core 1 on the landing zone, overwriting 12 bytes
        // at 0x580000 in the sense core's own SRAM. The alternative -- leaving the core
        // dead -- makes it undebuggable, so pay the side effect.
        if !self.enable_core1_debug(interface, ap) {
            // Containing this error matters: when `Session` walks every core it only
            // tolerates `CoreDisabled`, so letting anything else out of here fails
            // session creation and takes cm33_core0 down with a core we were only trying
            // to bring up as a bonus.
            if let Err(error) = self.enable_cpu1(interface, ap.dp()) {
                tracing::debug!("could not release cm33_core1 from reset: {error}");
            }

            if !self.enable_core1_debug(interface, ap) {
                tracing::warn!(
                    "cm33_core1 did not respond after being released from reset; \
                     reporting it as disabled. Only cm33_core0 will be available."
                );
                return Err(ArmError::CoreDisabled);
            }
        }

        DefaultArmSequence(()).on_attach(interface, ap, core_type, target)
    }

    fn reset_system(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::trace!("RT7xx reset system");

        // The sense core has no reset of its own. The only reset reachable from here is
        // SYSRESETREQ, which resets the whole chip and puts that core back under the boot
        // ROM's control, undoing the release that made it debuggable. Doing that behind
        // the back of someone who asked to reset core 1 would be worse than not
        // resetting, so halt the core and say what happened.
        if core.fully_qualified_address().ap() == &ApAddress::V1(Self::CORE1_AP) {
            tracing::warn!(
                "cm33_core1 has no core-local system reset on this part, so it was only \
                 halted. Reset cm33_core0 to reset the chip."
            );

            let mut dhcsr = Dhcsr(0);
            dhcsr.set_c_halt(true);
            dhcsr.set_c_debugen(true);
            dhcsr.enable_write();
            core.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
            core.flush()?;

            return Ok(());
        }

        // Halt the core.
        let mut dhcsr = Dhcsr(0);
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);
        dhcsr.enable_write();
        core.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

        // Execution restarts in the boot ROM, so a reset vector catch would fire there
        // rather than in the application -- and the ROM disables debug access while it
        // runs, which is what makes the stock `cortex_m_reset_system` fault on the DHCSR
        // read right after SYSRESETREQ. Drop the vector catch that `reset_catch_set`
        // installed, set TRCENA, and watch for the ROM's read of SYSTEM_STICK_CALIB
        // instead; that happens once the ROM is nearly done.
        let mut demcr: Demcr = core.read_word_32(Demcr::get_mmio_address())?.into();
        demcr.set_trcena(true);
        demcr.set_vc_corereset(false);
        core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

        core.write_word_32(Self::DWT_COMP0, Self::SYSTEM_STICK_CALIB_ADDR)?;

        let mut function = DwtFunction(0);
        function.set_datavsize(0b10);
        function.set_action(0b01);
        function.set_match(0b0100);
        core.write_word_32(Self::DWT_FUNCTION0, function.0)?;
        core.flush()?;

        // Execute SYSRESETREQ. The reset tears the connection down mid-transaction, so
        // the write and flush are expected to look like they failed.
        let mut aircr = Aircr(0);
        aircr.set_sysresetreq(true);
        aircr.vectkey();
        let _ = core.write_word_32(Aircr::get_mmio_address(), aircr.into());
        let _ = core.flush();

        thread::sleep(Self::BOOT_TIME);

        self.wait_for_stop_after_reset(core)
    }
}

#[cfg(test)]
mod test {
    use super::{DwtFunction, MIMXRT7xx};
    use crate::architecture::arm::core::registers::cortex_m::XPSR;

    /// NXP's own debug description arms the reset catch watchpoint with a bare
    /// `DWT_FUNCTION0 = 0x0000_0814`. Pin the field decomposition to that value, so that
    /// a mistake in the bitfield cannot silently change what we arm.
    #[test]
    fn dwt_function_arms_word_read_watchpoint() {
        let mut function = DwtFunction(0);
        function.set_datavsize(0b10);
        function.set_action(0b01);
        function.set_match(0b0100);

        assert_eq!(function.0, 0x0000_0814);
    }

    /// Likewise for the two `DCRSR` writes it does, `0x0001_0010` and `0x0001_001C`: the
    /// low byte is `REGSEL`, so these have to be XPSR and MSPLIM_S.
    #[test]
    fn core_register_selectors_are_xpsr_and_msplim() {
        assert_eq!(XPSR.id.0, 0x0010);
        assert_eq!(MIMXRT7xx::REGSEL_MSPLIM_S, 0x001C);
    }
}
