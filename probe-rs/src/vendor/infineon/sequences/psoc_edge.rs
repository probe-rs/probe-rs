//! Debug sequences for PSOC Edge devices.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};

use bitfield::bitfield;
use probe_rs_target::{Chip, CoreType};

use crate::{
    architecture::arm::{
        ApV2Address, ArmDebugInterface, ArmError, DapAccess, FullyQualifiedApAddress,
        armv8m::Dhcsr,
        dp::{Ctrl, DpAddress, DpRegister},
        memory::{ArmMemoryInterface, MemoryAccessSecurityPolicy},
        sequences::{ArmDebugSequence, DefaultArmSequence},
        traits::DebugPortWire,
    },
    config::CoreExt,
    core::memory_mapped_registers::MemoryMappedRegister,
    probe::WireProtocol,
};

use super::common::{
    self, AP_CSW, AP_DRW, AP_TAR, cortex_m_reset_system_with_recovery,
    cortex_m_wait_for_reset_with_recovery,
};

bitfield! {
    /// CM55 control register.
    #[derive(Clone, Copy)]
    struct MxCm55Ctl(u32);
    impl Debug;

    /// Holds CM55 until CM33 has initialized its system and vector table.
    pub cm55_wait, set_cm55_wait: 4;
}
impl MxCm55Ctl {
    /// Secure CM55 control-register alias.
    const ADDRESS: u64 = 0x54160000;
}

bitfield! {
    /// CM55 local command register.
    #[derive(Clone, Copy)]
    struct MxCm55Cmd(u32);
    impl Debug;
}
impl MxCm55Cmd {
    /// Secure CM55 command-register alias.
    const ADDRESS: u64 = 0x54160004;
    /// Reset command with the required key.
    const RESET: u32 = 0x05FA_0001;
}

bitfield! {
    /// Application CPU subsystem AP control register.
    #[derive(Clone, Copy)]
    struct AppCpussApCtl(u32);
    impl Debug;

    /// Enables the CM55 debug access port.
    pub cm55_enable, set_cm55_enable: 0;

    /// Enables invasive CM55 debug access.
    pub cm55_dbg_enable, set_cm55_dbg_enable: 4;

    /// Enables non-invasive CM55 debug access.
    pub cm55_nid_enable, set_cm55_nid_enable: 5;
}
impl AppCpussApCtl {
    const ADDRESS: u64 = 0x441C1000;
}

bitfield! {
    /// CM55 power-domain sense register.
    #[derive(Clone, Copy)]
    struct PwrmodePd6PdSense(u32);
    impl Debug;

    /// Powers on the CM55 domain.
    pub pd_on_cm55, set_pd_on_cm55: 4;
    pub a, set_a: 31;
}
impl PwrmodePd6PdSense {
    const ADDRESS: u64 = 0x42410060;
}

const SECURE_ALIAS_MSK: u32 = 0x1000_0000;
const PPC0_BASE: u64 = 0x5202_0000;
const PPC0_MAX_REGIONS: u32 = 0x13F;
const PPC1_BASE: u64 = 0x5402_0000;
const PPC1_MAX_REGIONS: u32 = 0xA4;
const PPC_PC_MASK_OFF: u64 = 0x1000;
const PPC_NS_ATT_OFF: u64 = 0x2000;
const PPC_R_ADDR_OFF: u64 = 0x5000;
const PPC_R_ATT_OFF: u64 = 0x6000;

/// SYS AP CSW for secure 32-bit word accesses.
const SYS_AP_CSW_WORD: u32 = 0xAB00_0012;

pub(super) const CM55_AP_BASE: u64 = 0xF000_6000;

/// CM33 AP CSW for secure 32-bit word accesses (`HNONSEC = 0`).
/// Used for raw transfers because the running AP can report `IDR == 0`.
const CM33_AP_CSW_WORD: u32 = 0xBB00_0012;
const CM33_AP_IDR: u32 = 0x3477_0008;

/// CM55 AP CSW for secure 32-bit word accesses (`HNONSEC = 0`).
/// Used for raw transfers because the running AP can report `IDR == 0`.
const CM55_AP_CSW_WORD: u32 = 0xFB00_0012;

/// A secure (HNONSEC=0) access to a non-secure alias faults and drops the CM33 AP over JTAG.
fn cm33_security_policy(address: u64) -> bool {
    matches!((address >> 24) as u8, 0x42..=0x49)
}

fn cm33_memory_interface<'a>(
    interface: &'a mut dyn ArmDebugInterface,
    cm33_ap: &FullyQualifiedApAddress,
) -> Result<Box<dyn ArmMemoryInterface + 'a>, ArmError> {
    interface.memory_interface_with_security_policy(cm33_ap, Some(cm33_security_policy))
}

/// Test Mode Control Register (`SRSS->TST_MODE`).
const TST_MODE_ADDRESS: u32 = 0x5240_0400;

/// `TST_MODE.TEST_MODE` (bit 31): request the boot ROM to stay in its Listen Window.
const TEST_MODE_MSK: u32 = 0x8000_0000;

/// Maximum time to wait for the boot ROM to reach its idle Listen Window.
const TIMEOUT_BOOT_COMPLETE_MS: u64 = 5000;

/// How long to hold XRES (nRESET) low during a reset-acquire.
const XRES_HOLD_MS: u64 = 10;

/// Extra time for the boot ROM to re-enable SWJ after XRES release.
const RESET_HANDSHAKE_MS: u64 = 100;

/// Delay required before debug access after reset.
const RESET_FINISH_DELAY_MS: u64 = 500;
const POST_CM55_ENABLE_SETTLE_MS: u64 = 10;

/// PSOC Edge debug sequences.
#[derive(Debug)]
pub struct PsocEdge {
    // The access port for the system CPU (CM33).
    cm33_ap: FullyQualifiedApAddress,

    // The access port for the application CPU (CM55).
    cm55_ap: FullyQualifiedApAddress,

    // Whether the CM55 debug interface was successfully enabled. Used to decide
    // whether to re-enable it after a system reset.
    cm55_enabled: AtomicBool,

    // Whether the active wire protocol is JTAG. Captured in `debug_port_setup`.
    is_jtag: AtomicBool,

    // Saved DEMCR values for exact reset-catch restore semantics per core.
    cm33_demcr_saved: AtomicU32,
    cm55_demcr_saved: AtomicU32,
    cm33_demcr_valid: AtomicBool,
    cm55_demcr_valid: AtomicBool,

    // Whether the JTAG recovery path has already escalated to an XRES reset-acquire for
    // the current degradation. Cleared once the CM33 AP reads healthy again.
    xres_recovery_used: AtomicBool,
}

impl PsocEdge {
    /// Creates debug sequences for a PSOC Edge chip.
    pub fn create(chip: &Chip) -> Arc<Self> {
        let [cm33, cm55] = &*chip.cores else {
            unreachable!("PSOC Edge E84 devices have two cores");
        };
        let [cm33_ap, cm55_ap] = [cm33, cm55].map(|core| {
            core.memory_ap()
                .expect("PSOC Edge core must have a memory AP")
        });

        Arc::new(PsocEdge {
            cm33_ap,
            cm55_ap,
            cm55_enabled: AtomicBool::new(false),
            is_jtag: AtomicBool::new(false),
            cm33_demcr_saved: AtomicU32::new(0),
            cm55_demcr_saved: AtomicU32::new(0),
            cm33_demcr_valid: AtomicBool::new(false),
            cm55_demcr_valid: AtomicBool::new(false),
            xres_recovery_used: AtomicBool::new(false),
        })
    }
}

impl ArmDebugSequence for PsocEdge {
    fn memory_security_policy(
        &self,
        ap: &FullyQualifiedApAddress,
    ) -> Option<MemoryAccessSecurityPolicy> {
        // CM55 is left on its inherited secure CSW until the same alias split can be
        // confirmed on hardware for that AP.
        (*ap == self.cm33_ap).then_some(cm33_security_policy)
    }

    fn on_attach(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        core_type: CoreType,
    ) -> Result<(), ArmError> {
        if core_ap == &self.cm33_ap {
            tracing::debug!("PSOC Edge: attaching to CM33 core");
            self.ensure_jtag_cm33_ap_ready(interface, core_ap.dp())?;
            tracing::debug!("PSOC Edge: done CM33 attach");
        }

        if core_ap == &self.cm55_ap {
            tracing::debug!("PSOC Edge: attaching to CM55 core");

            let dp = core_ap.dp();

            // If the CM55 has already been enabled, do NOT re-run the full power-up /
            // CPU_WAIT-release sequence on every attach. `session.core(1)` calls this
            // on each access (connect, `info threads`, register reads), and re-driving
            // that sequence re-writes DHCSR and can resume a core the debugger had
            // already halted, which then makes register reads fail. Instead just
            // confirm the debug interface is still live and (re-)halt the core.
            if self.cm55_enabled.load(Ordering::Relaxed) {
                match self.ensure_cm55_halted(interface) {
                    Ok(()) => {
                        // The first attach already performed the generic CM55 setup. On
                        // re-attach, repeating it can issue another AP transaction after
                        // the core is halted and poison the shared JTAG DP.
                        return Ok(());
                    }
                    Err(e) => {
                        // Interface no longer responding — fall back to a full re-enable.
                        tracing::debug!("CM55 re-attach halt check failed: {:?}", e);
                        common::recover_dp(interface, dp);
                        self.cm55_enabled.store(false, Ordering::Relaxed);
                    }
                }
            }

            // Power up / attach the CM55 via the CM33 AP. This runs after CM33 is fully
            // enumerated and after debug_device_unlock's CM33 recovery, so it is the only
            // place the power-up survives; on failure it recovers the CM33 AP.
            match self.try_enable_cm55(interface) {
                Ok(()) => {}
                Err(ArmError::CoreDisabled) => {
                    return Err(ArmError::CoreDisabled);
                }
                Err(e) => {
                    // A firmware-owned CM55 may reject debug-domain access. Treat that as
                    // an unavailable secondary core so CM33 operations can continue; do
                    // not recover the shared DP here because that can disrupt CM33.
                    tracing::debug!("CM55 enable failed during attach: {:?}", e);
                    return Err(ArmError::CoreDisabled);
                }
            }
            tracing::debug!("PSOC Edge: done CM55 attach");
        }

        tracing::debug!(
            "PSOC Edge: start default attach sequence for core {:?}",
            core_type
        );
        let result = DefaultArmSequence(()).on_attach(interface, core_ap, core_type);
        tracing::debug!("PSOC Edge: done default attach for core {:?}", core_type);
        result
    }

    /// Prepare the debug port for connection, waking the SWJ-DP from its dormant
    /// state when JTAG is selected.
    ///
    /// JTAG requires an explicit dormant-to-JTAG alert and TAP confirmation.
    /// SWD uses the standard setup with a dormant fallback.
    fn debug_port_setup(
        &self,
        interface: &mut dyn DebugPortWire,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        let is_jtag = interface.active_protocol() == Some(WireProtocol::Jtag);
        self.is_jtag.store(is_jtag, Ordering::Relaxed);
        if !is_jtag {
            // Retry after XRES because the first boot-ROM debug window may have closed.
            tracing::debug!("PSOC Edge: SWD dormant connect phase 1 (400ms)");
            if common::try_swd_dormant_connect(interface, std::time::Duration::from_millis(400))? {
                return DefaultArmSequence(()).debug_port_connect(interface, dp);
            }

            match interface.target_reset() {
                Ok(()) => tracing::warn!("PSOC Edge: XRES pulsed for SWD reconnect"),
                Err(e) => tracing::debug!("PSOC Edge: XRES unavailable for SWD reconnect: {e}"),
            }
            std::thread::sleep(std::time::Duration::from_millis(10));

            tracing::debug!("PSOC Edge: SWD dormant connect phase 2 (1200ms)");
            if common::try_swd_dormant_connect(interface, std::time::Duration::from_millis(1200))? {
                return DefaultArmSequence(()).debug_port_connect(interface, dp);
            }

            tracing::warn!("PSOC Edge: SWD dormant reconnect failed after XRES");
            return DefaultArmSequence(()).debug_port_setup(interface, dp);
        }

        // JTAG: wake the DP from dormant and select JTAG, confirm the TAP, then connect;
        // on failure fall back to the default sequence so a proper error surfaces.
        // `None` → the default `__Reset_Finish_Delay` boot-window budget.
        common::jtag_dormant_wake_or_default(interface, dp, None)
    }

    /// Power up the debug port, falling back to an XRES reset-acquire if the port is
    /// locked by running firmware.
    ///
    /// If power-up fails because firmware gated the DP, pulse XRES and latch Test Mode
    /// while the boot ROM's Listen Window is open.
    fn debug_port_start(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        // Use the direct power-up path first. Recover a degraded CM33 AP later, after
        // debug-device setup has identified the AP type.
        //
        // JTAG clears sticky bits through CTRL/STAT; SWD uses ABORT.
        let start_result = common::dp_start_with_powerup(interface, dp, false, false);
        match start_result {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::debug!(
                    "PSOC Edge: debug port power-up failed ({:?}); attempting XRES reset-acquire",
                    e
                );
                self.reset_acquire_debug_port(interface, dp, e)
            }
        }
    }

    fn debug_port_stop(
        &self,
        interface: &mut dyn DebugPortWire,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        let result = DefaultArmSequence(()).debug_port_stop(interface, dp);
        common::enter_dormant(interface);
        result
    }

    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        // Best-effort Test Mode acquisition keeps the boot ROM in its Listen Window.
        // Failures are non-fatal; clear sticky errors and continue.
        let dp = default_ap.dp();

        self.ensure_jtag_cm33_ap_ready(interface, dp)?;

        // Minimal SYS-AP init only: no Test Mode / XRES reset-acquire escalation.
        let sys_ap = common::sys_ap(dp);
        if let Err(e) = interface.write_raw_ap_register(&sys_ap, AP_CSW, SYS_AP_CSW_WORD) {
            tracing::debug!("PSOC Edge: SYS AP init failed: {:?}", e);
            common::recover_dp(interface, dp);
        }

        let cm33 = common::cm33(dp);
        if let Err(e) = interface.write_raw_ap_register(&cm33, AP_CSW, CM33_AP_CSW_WORD) {
            tracing::debug!("PSOC Edge: CM33 AP init failed: {:?}", e);
            common::recover_dp(interface, dp);
        }

        let cm55 = FullyQualifiedApAddress::v2_with_dp(dp, ApV2Address(Some(CM55_AP_BASE)));
        if let Err(e) = interface.write_raw_ap_register(&cm55, AP_CSW, CM55_AP_CSW_WORD) {
            tracing::debug!("PSOC Edge: CM55 AP init failed: {:?}", e);
            common::recover_dp(interface, dp);
        }

        Ok(())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let ap = interface.fully_qualified_address();

        if ap == self.cm55_ap {
            // Local CM55 reset: write CM55_CMD reset value via the CM33 AP so that only
            // the CM55 is reset, leaving CM33 and the rest of the system untouched.
            // The CMD register lives in the APPSS secure address space; CM55 AHB can
            // also reach it but the CM33 AP is the safer path since CM55 may be halted.
            tracing::debug!("PSOC Edge: local CM55 reset via CM33 AP");

            let local_reset_result = (|| -> Result<(), ArmError> {
                {
                    let arm = interface
                        .get_arm_debug_interface()
                        .map_err(ArmError::Probe)?;
                    let cmd_addr =
                        Self::ppc_resolve_addr(arm, &self.cm33_ap, MxCm55Cmd::ADDRESS as u32)
                            .map(u64::from)
                            .unwrap_or(MxCm55Cmd::ADDRESS);
                    let mut cm33_ap = cm33_memory_interface(arm, &self.cm33_ap)?;
                    cm33_ap.write_word_32(cmd_addr, MxCm55Cmd::RESET)?;
                }

                // Wait for CM55 to come back up by polling DHCSR via the CM55 AP.
                cortex_m_wait_for_reset_with_recovery(interface, |interface| {
                    self.reconnect_after_reset(interface)
                })?;
                Ok(())
            })();

            if let Err(e) = local_reset_result {
                tracing::warn!(
                    "PSOC Edge: local CM55 reset failed ({:?}), falling back to SYSRESETREQ",
                    e
                );
                let dp = self.cm33_ap.dp();
                match interface.get_arm_debug_interface() {
                    Ok(arm) => common::recover_dp(arm, dp),
                    Err(err) => tracing::warn!(
                        "PSOC Edge: could not obtain ARM debug interface to recover DP: {:?}",
                        err
                    ),
                }
                cortex_m_reset_system_with_recovery(interface, |interface| {
                    self.reconnect_after_reset(interface)
                })?;
            }

            // Re-enable CM55 debug AP access (local reset and system reset both clear AppCpussApCtl).
            self.cm55_enabled.store(false, Ordering::Relaxed);
            let enable_result = {
                let arm = interface
                    .get_arm_debug_interface()
                    .map_err(ArmError::Probe)?;
                self.try_enable_cm55(arm)
            };
            match enable_result {
                Ok(()) => {}
                Err(ArmError::CoreDisabled) => {
                    tracing::debug!("PSOC Edge: CM55 not ready after reset");
                }
                Err(e) => {
                    let dp = self.cm33_ap.dp();
                    match interface.get_arm_debug_interface() {
                        Ok(arm) => common::recover_dp(arm, dp),
                        Err(err) => tracing::warn!(
                            "PSOC Edge: could not obtain ARM debug interface to recover DP: {:?}",
                            err
                        ),
                    }
                    return Err(e);
                }
            }
            return Ok(());
        }

        // CM33 (system) reset path.
        //
        // Use the Armv8-M AIRCR reset for both transports. The SRSS soft reset can
        // leave CM33's memory protection state incompatible with the flash algorithm's
        // stack after a JTAG reset, even though AP memory accesses remain healthy.
        // Clear DHCSR.S_RESET_ST so the upcoming reset can be observed.
        let _ = interface.read_word_32(Dhcsr::get_mmio_address());

        tracing::debug!("PSOC Edge: system reset via AIRCR.SYSRESETREQ");
        if let Err(reset_error) = cortex_m_reset_system_with_recovery(interface, |interface| {
            self.reconnect_after_reset(interface)
        }) {
            tracing::warn!(
                "PSOC Edge: AIRCR reset interrupted the AP transaction; reconnecting: {:?}",
                reset_error
            );
            let arm = interface
                .get_arm_debug_interface()
                .map_err(ArmError::Probe)?;
            arm.reinitialize()?;
            self.reconnect_after_reset(interface)
                .map_err(|_| reset_error)?;
        }

        // Give secure boot time to re-run before continuing with the CM33 algorithm.
        std::thread::sleep(std::time::Duration::from_millis(RESET_FINISH_DELAY_MS));

        // A CM33 system reset reinitializes the CM55 domain and clears its debug AP
        // enable bits, even when this sequence instance never attached CM55 before
        // reset. Restore the AP unconditionally so a later GDB thread switch does not
        // attempt register access through a disabled CM55 AP.
        self.cm55_enabled.store(false, Ordering::Relaxed);
        tracing::debug!("PSOC Edge: re-enabling CM55 debug access after system reset");
        let arm = interface
            .get_arm_debug_interface()
            .map_err(ArmError::Probe)?;
        match self.try_enable_cm55(arm) {
            Ok(()) => {}
            Err(ArmError::CoreDisabled) => {
                tracing::debug!("PSOC Edge: CM55 not ready after system reset");
            }
            Err(error) => {
                // The CM33 reset already succeeded; a CM55 rejecting debug access must not fail it.
                tracing::debug!("PSOC Edge: CM55 re-enable after system reset failed: {error:?}");
                common::recover_dp(arm, self.cm33_ap.dp());
            }
        }
        // The CM55 domain/AP enable writes are posted through CM33. Give the domain
        // time to finish coming out of reset before a GDB thread switch or algorithm
        // pass uses the shared IRAM2 and CM33 stack.
        std::thread::sleep(std::time::Duration::from_millis(POST_CM55_ENABLE_SETTLE_MS));

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
        if core_ap == &self.cm55_ap {
            // CM55 enablement is deferred to on_attach() which has proper error recovery
            // for the AHB stall that can occur when accessing CM55 registers in boot ROM state.
            return Ok(());
        }
        DefaultArmSequence(()).debug_core_start(interface, core_ap, core_type, debug_base, cti_base)
    }

    fn reset_catch_set(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::armv8m::Demcr;

        let ap = core.fully_qualified_address();
        let current_demcr = core.read_word_32(Demcr::get_mmio_address())?;

        if ap == self.cm33_ap {
            self.cm33_demcr_saved
                .store(current_demcr, Ordering::Relaxed);
            self.cm33_demcr_valid.store(true, Ordering::Relaxed);
        } else if ap == self.cm55_ap {
            self.cm55_demcr_saved
                .store(current_demcr, Ordering::Relaxed);
            self.cm55_demcr_valid.store(true, Ordering::Relaxed);
        }

        let mut demcr = Demcr(current_demcr);
        demcr.set_vc_corereset(true);
        core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

        // Clear S_RESET_ST side effects on write by reading DHCSR.
        let _ = core.read_word_32(Dhcsr::get_mmio_address())?;

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::armv8m::Demcr;

        let ap = core.fully_qualified_address();
        let restored = if ap == self.cm33_ap && self.cm33_demcr_valid.load(Ordering::Relaxed) {
            self.cm33_demcr_valid.store(false, Ordering::Relaxed);
            Some(self.cm33_demcr_saved.load(Ordering::Relaxed))
        } else if ap == self.cm55_ap && self.cm55_demcr_valid.load(Ordering::Relaxed) {
            self.cm55_demcr_valid.store(false, Ordering::Relaxed);
            Some(self.cm55_demcr_saved.load(Ordering::Relaxed))
        } else {
            None
        };

        if let Some(previous_demcr) = restored {
            core.write_word_32(Demcr::get_mmio_address(), previous_demcr)?;
            return Ok(());
        }

        // Fallback: no saved state for this core, so clear reset-catch bit only.
        let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
        demcr.set_vc_corereset(false);
        core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
        Ok(())
    }
}

impl PsocEdge {
    fn reconnect_after_reset(
        &self,
        interface: &mut dyn ArmMemoryInterface,
    ) -> Result<(), ArmError> {
        let dp = self.cm33_ap.dp();
        let is_jtag = self.is_jtag.load(Ordering::Relaxed);
        let arm = interface
            .get_arm_debug_interface()
            .map_err(ArmError::Probe)?;

        let mut connected = false;
        match arm.debug_port_reconnect_with(&mut |dap| {
            connected = if is_jtag {
                common::try_jtag_dormant_wake(dap, Some(std::time::Duration::from_millis(1200)))?
            } else {
                common::try_swd_dormant_connect(dap, std::time::Duration::from_millis(1200))?
            };
            if connected {
                DefaultArmSequence(()).debug_port_connect(dap, dp)?;
            }
            Ok(())
        }) {
            Ok(()) | Err(ArmError::NotImplemented(_)) => {}
            Err(e) => return Err(e),
        }
        if !connected {
            return Err(ArmError::CoreDisabled);
        }
        if is_jtag {
            common::jtag_dp_start_with_powerup(arm, dp, true, true)?;
        } else {
            common::swd_dp_start_with_powerup(arm, dp)?;
        }

        Ok(())
    }

    fn ensure_jtag_cm33_ap_ready(
        &self,
        interface: &mut dyn ArmDebugInterface,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        if !self.is_jtag.load(Ordering::Relaxed) {
            return Ok(());
        }

        if self.cm33_ap_is_healthy(interface, dp) {
            self.xres_recovery_used.store(false, Ordering::Relaxed);
            return Ok(());
        }

        tracing::warn!(
            "PSOC Edge: CM33 AP not responding over JTAG; clearing sticky errors and retrying"
        );
        let _ = interface.debug_port_reconnect_with(&mut |probe| {
            if let Err(error) = probe.configure_jtag(false) {
                tracing::debug!("PSOC Edge: JTAG reconfiguration during recovery failed: {error}");
            } else if let Some(mut jtag) = probe.try_jtag_chain() {
                jtag.set_chain(&[]);
                match jtag.scan_chain().map(|c| c.len()) {
                    Ok(n) if n > 0 => {
                        if let Err(error) = jtag.select(0) {
                            tracing::debug!(
                                "PSOC Edge: selecting JTAG TAP 0 during recovery failed: {error}"
                            );
                        }
                    }
                    Ok(_) => tracing::debug!("PSOC Edge: JTAG recovery scan found no TAPs"),
                    Err(error) => tracing::debug!("PSOC Edge: JTAG recovery scan failed: {error}"),
                }
            }
            Ok(())
        });
        let _ = common::jtag_dp_start_with_powerup(interface, dp, true, true);
        common::recover_dp(interface, dp);
        if self.cm33_ap_is_healthy(interface, dp) {
            tracing::debug!("PSOC Edge: CM33 AP reachable again without reset");
            self.xres_recovery_used.store(false, Ordering::Relaxed);
            return Ok(());
        }

        // A reset-acquire restarts the boot ROM, which leaves the AP looking unreachable
        // again on the next check, so repeating it here would spin until the session
        // times out. CM33 is always-on: if the probe still cannot see it, let the attach
        // continue so the real operation reports the real error.
        if self.xres_recovery_used.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "PSOC Edge: CM33 AP still unreachable over JTAG after an XRES \
                 reset-acquire; continuing attach"
            );
            return Ok(());
        }

        tracing::warn!(
            "PSOC Edge: CM33 AP unreachable over JTAG; using XRES reset-acquire before attach"
        );
        self.reset_acquire_debug_port(interface, dp, ArmError::CoreDisabled)?;

        let sys_ap = common::sys_ap(dp);
        interface.write_raw_ap_register(&sys_ap, AP_CSW, SYS_AP_CSW_WORD)?;
        interface.write_raw_ap_register(&self.cm33_ap, AP_CSW, CM33_AP_CSW_WORD)?;
        Ok(())
    }

    /// Liveness probe for the JTAG path to the CM33 AP.
    ///
    /// Sticky error bits latch from any earlier faulting transaction — including the
    /// CM55 probing this sequence performs deliberately — so they are cleared first
    /// rather than treated as a verdict on the CM33 AP.
    fn cm33_ap_is_healthy(&self, interface: &mut dyn ArmDebugInterface, dp: DpAddress) -> bool {
        const AP_IDR: u64 = 0xDFC;

        const STICKY_W1C: u32 = 0x32; // STICKYERR | STICKYCMP | STICKYORUN
        const KEEP_MASK: u32 = 0x5000_0001; // CSYSPWRUPREQ | CDBGPWRUPREQ | ORUNDETECT

        let Ok(ctrl) = interface.read_raw_dp_register(dp, Ctrl::ADDRESS) else {
            return false;
        };
        // With STICKYERR latched the JTAG-DP discards AP accesses, so the IDR read below
        // would fault regardless of the AP's state.
        if ctrl & STICKY_W1C != 0 {
            tracing::debug!(
                "PSOC Edge: clearing latched DP sticky errors (CTRL/STAT={ctrl:#010x})"
            );
            if interface
                .write_raw_dp_register(dp, Ctrl::ADDRESS, (ctrl & KEEP_MASK) | STICKY_W1C)
                .is_err()
            {
                return false;
            }
        }
        let Ok(idr) = interface.read_raw_ap_register(&self.cm33_ap, AP_IDR) else {
            return false;
        };
        let Ok(ctrl) = interface.read_raw_dp_register(dp, Ctrl::ADDRESS) else {
            return false;
        };

        !Ctrl(ctrl).sticky_err() && idr == CM33_AP_IDR
    }

    fn write_cm33_control_with_recovery(
        &self,
        interface: &mut dyn ArmDebugInterface,
        address: u64,
        value: u32,
        name: &str,
    ) -> Result<(), ArmError> {
        const MAX_ATTEMPTS: usize = 2;
        let dp = self.cm33_ap.dp();
        let is_jtag = self.is_jtag.load(Ordering::Relaxed);

        for attempt in 1..=MAX_ATTEMPTS {
            let write_result = {
                let mut cm33_ap = cm33_memory_interface(interface, &self.cm33_ap)?;
                cm33_ap.write_word_32(address, value)
            };

            if let Err(error) = write_result {
                if !is_jtag || attempt == MAX_ATTEMPTS {
                    return Err(error);
                }

                tracing::warn!(
                    "PSOC Edge: {name} write failed over JTAG; recovering before retry {attempt}/{MAX_ATTEMPTS}: {error:?}"
                );
                self.ensure_jtag_cm33_ap_ready(interface, dp)?;
                continue;
            }

            if !is_jtag || self.cm33_ap_is_healthy(interface, dp) {
                return Ok(());
            }

            tracing::warn!(
                "PSOC Edge: CM33 AP degraded after {name} write; recovering before retry {attempt}/{MAX_ATTEMPTS}"
            );
            self.ensure_jtag_cm33_ap_ready(interface, dp)?;
        }

        Err(ArmError::CoreDisabled)
    }

    /// Recovery path for [`debug_port_start`](Self::debug_port_start): pulse XRES and
    /// race the boot ROM's Listen Window to bring the debug port up on a device whose
    /// running firmware has gated it.
    ///
    /// Assert nRESET (XRES) low, release it, then repeatedly re-establish the DP
    /// connection and retry the standard power-up until it succeeds or the
    /// boot-complete timeout elapses. Once the DP is up, Test Mode is latched via the
    /// SYS AP so the boot ROM keeps holding in its Listen Window instead of handing
    /// control back to the (gating) firmware.
    /// Returns `original_err` unchanged if the probe cannot drive nRESET, so a probe
    /// without reset control fails exactly as it did before.
    fn reset_acquire_debug_port(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
        original_err: ArmError,
    ) -> Result<(), ArmError> {
        // Reset the device with XRES so the boot ROM restarts and re-opens its Listen
        // Window.
        //
        // Pulse XRES through the SWJ nRESET pin. This restarts the boot ROM and
        // re-opens its Listen Window; the generic target-reset command is not reliable
        // for this device.
        {
            // Hold XRES low long enough for the device to enter reset, then release it
            // so the boot ROM starts running and opens its Listen Window.
            let mut pulsed = false;
            if interface
                .debug_port_reconnect_with(&mut |probe| {
                    pulsed = common::pulse_xres_for(probe, XRES_HOLD_MS);
                    Ok(())
                })
                .is_err()
            {
                tracing::warn!(
                    "PSOC Edge: no raw probe available; cannot pulse XRES to recover the debug port"
                );
                return Err(original_err);
            }
            if !pulsed {
                tracing::warn!(
                    "PSOC Edge: probe cannot control nRESET; XRES reset-acquire unavailable"
                );
                return Err(original_err);
            }
        }

        // Let the boot ROM re-enable the SWJ pins before the first TAP scan. Without
        // this settle the initial post-reset scan runs while the TAP is still down and
        // fails with "did not detect 1" (logged at ERROR by the shared cmsisdap layer),
        // making a successful recovery look alarming.
        std::thread::sleep(std::time::Duration::from_millis(RESET_HANDSHAKE_MS));

        let sys_ap = common::sys_ap(dp);
        let timeout =
            std::time::Duration::from_millis(TIMEOUT_BOOT_COMPLETE_MS + RESET_HANDSHAKE_MS);

        // Race the boot ROM's Listen Window. The window (DAP enabled, right after secure
        // boot) is narrow, so keep the per-iteration work minimal: re-scan the JTAG TAP
        // only when needed, and otherwise tight-hammer the lightweight CTRL/STAT power-up
        // + DPIDR read so we hit the window the instant the boot ROM opens it. `ZERO`
        // poll interval: hammer with no delay to hit the window as soon as it opens.
        let mut need_setup = true;
        let mut failures: u32 = 0;
        let powered = common::retry_until_deadline(timeout, std::time::Duration::ZERO, || {
            // XRES also resets the debug port. Re-establish the TAP once (and again only
            // after a run of failures, in case the reset dropped it), not every pass.
            if need_setup {
                let setup_succeeded = interface
                    .debug_port_reconnect_with(&mut |probe| self.debug_port_setup(probe, dp))
                    .is_ok();
                if !setup_succeeded {
                    // Do not send DP/AP transactions while the probe has no valid TAP
                    // configuration. In that state ABORT/CTRL-STAT accesses produce
                    // misleading IR-length errors instead of a useful NACK.
                    failures += 1;
                    return Ok(false);
                }
                need_setup = false;
            }

            // Power up the DAP via CTRL/STAT before reading DPIDR.
            common::jtag_dp_powerup(interface, dp, false);

            match DefaultArmSequence(()).debug_port_start(&mut *interface, dp) {
                Ok(()) => {
                    // The DP is up. Latch Test Mode so the boot ROM keeps holding in its
                    // Listen Window instead of handing control to (gating) firmware.
                    let _ = interface.write_raw_ap_register(&sys_ap, AP_CSW, SYS_AP_CSW_WORD);
                    let _ = interface.write_raw_ap_register(&sys_ap, AP_TAR, TST_MODE_ADDRESS);
                    let _ = interface.write_raw_ap_register(&sys_ap, AP_DRW, TEST_MODE_MSK);
                    tracing::warn!(
                        "PSOC Edge: debug port powered up after XRES reset (Test Mode latched)"
                    );
                    Ok(true)
                }
                Err(_) => {
                    // Not up yet — clear any sticky error and keep racing the window.
                    common::recover_dp(interface, dp);
                    failures += 1;
                    // Periodically re-scan the TAP in case the reset dropped it entirely.
                    if failures.is_multiple_of(64) {
                        need_setup = true;
                    }
                    Ok(false)
                }
            }
        })?;

        if powered {
            return Ok(());
        }

        tracing::warn!(
            "PSOC Edge: debug port did not power up after XRES reset within {}ms",
            TIMEOUT_BOOT_COMPLETE_MS + RESET_HANDSHAKE_MS
        );
        // Surface the real power-up error from a final attempt.
        DefaultArmSequence(()).debug_port_start(interface, dp)
    }

    fn ppc_table(target_addr: u32) -> Option<(u32, u64, u32)> {
        let addr = target_addr & !SECURE_ALIAS_MSK;
        let prefix = (addr >> 24) & 0xFF;
        if (0x42..=0x43).contains(&prefix) {
            Some((addr, PPC0_BASE, PPC0_MAX_REGIONS))
        } else if (0x44..=0x45).contains(&prefix) || (0x48..=0x49).contains(&prefix) {
            Some((addr, PPC1_BASE, PPC1_MAX_REGIONS))
        } else {
            None
        }
    }

    fn ppc_read_table(mem: &mut dyn ArmMemoryInterface, address: u64, len: u32) -> Vec<u32> {
        let mut table = vec![0; len as usize];
        if let Err(error) = mem.read_32(address, &mut table) {
            tracing::debug!("PSOC Edge: PPC table read at {address:#010x} failed: {error:?}");
            table.fill(0);
        }
        table
    }

    fn ppc_find_region(addr: u32, r_addr: &[u32], r_att: &[u32]) -> Option<u64> {
        let addr = u64::from(addr);
        r_addr
            .iter()
            .zip(r_att)
            .position(|(&region_addr, &region_attr)| {
                let region_size = (region_attr >> 24) & 0x1F;
                let start = u64::from(region_addr & 0xFFFF_FFFC);
                region_size > 0 && addr >= start && addr < start + (2u64 << region_size)
            })
            .map(|index| index as u64)
    }

    fn ppc_resolve_addr(
        interface: &mut dyn ArmDebugInterface,
        cm33_ap: &FullyQualifiedApAddress,
        target_addr: u32,
    ) -> Option<u32> {
        let [resolved] = Self::ppc_resolve_addrs(interface, cm33_ap, [target_addr]);
        resolved
    }

    /// Resolve peripheral addresses to the secure or non-secure alias that the CM33 AP
    /// can currently reach: firmware programs the PPCs
    /// at boot, and each region carries a `PC_MASK` (is the debugger's protection
    /// context allowed at all?) and an `NS_ATT` bit (is the region non-secure?).
    /// Yields `None` when `PC_MASK` denies the debugger's protection context, meaning
    /// neither alias is usable and the caller must treat the peripheral as unreachable.
    fn ppc_resolve_addrs<const N: usize>(
        interface: &mut dyn ArmDebugInterface,
        cm33_ap: &FullyQualifiedApAddress,
        targets: [u32; N],
    ) -> [Option<u32>; N] {
        let mut resolved = targets.map(|target| Some(target & !SECURE_ALIAS_MSK));
        if targets
            .iter()
            .all(|&target| Self::ppc_table(target).is_none())
        {
            return resolved;
        }

        // Never cached: reset and firmware boot both reprogram the PPCs.
        let mut mem = cm33_memory_interface(interface, cm33_ap).ok();
        let mut tables: Vec<(u64, Vec<u32>, Vec<u32>)> = Vec::new();

        for (&target, out) in targets.iter().zip(resolved.iter_mut()) {
            let Some((addr, ppc_base, max_regions)) = Self::ppc_table(target) else {
                continue;
            };
            let Some(mem) = mem.as_deref_mut() else {
                *out = None;
                continue;
            };

            let table_index = match tables.iter().position(|(base, ..)| *base == ppc_base) {
                Some(index) => index,
                None => {
                    let r_addr = Self::ppc_read_table(mem, ppc_base + PPC_R_ADDR_OFF, max_regions);
                    let r_att = Self::ppc_read_table(mem, ppc_base + PPC_R_ATT_OFF, max_regions);
                    tables.push((ppc_base, r_addr, r_att));
                    tables.len() - 1
                }
            };
            let (_, r_addr, r_att) = &tables[table_index];

            let Some(region) = Self::ppc_find_region(addr, r_addr, r_att) else {
                *out = Some(addr | SECURE_ALIAS_MSK);
                continue;
            };
            let pc_mask = mem
                .read_word_32(ppc_base + PPC_PC_MASK_OFF + region * 4)
                .unwrap_or(0);
            if pc_mask & (1 << 2) == 0 {
                *out = None;
                continue;
            }

            let ns_att = mem
                .read_word_32(ppc_base + PPC_NS_ATT_OFF + (region / 32) * 4)
                .unwrap_or(0);
            *out = if ns_att & (1 << (region % 32)) == 0 {
                Some(addr | SECURE_ALIAS_MSK)
            } else {
                Some(addr)
            };
        }

        resolved
    }

    fn try_enable_cm55(&self, interface: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        // An access to the alias the PPC does not allow FAULTs and wedges the CM33 AP until XRES.
        let [pd_sense_addr, ap_ctl_addr, cm55_ctl_addr] = Self::ppc_resolve_addrs(
            interface,
            &self.cm33_ap,
            [
                PwrmodePd6PdSense::ADDRESS as u32,
                AppCpussApCtl::ADDRESS as u32,
                MxCm55Ctl::ADDRESS as u32,
            ],
        );
        let pd_sense_addr = pd_sense_addr
            .map(u64::from)
            .unwrap_or(PwrmodePd6PdSense::ADDRESS);
        let ap_ctl_addr = ap_ctl_addr.map(u64::from).unwrap_or(AppCpussApCtl::ADDRESS);
        let cm55_ctl_addr = cm55_ctl_addr.map(u64::from).unwrap_or(MxCm55Ctl::ADDRESS);
        tracing::debug!(
            "PSOC Edge: using PPC-resolved CM55 peripheral aliases: PD_SENSE={pd_sense_addr:#010x}, AP_CTL={ap_ctl_addr:#010x}, CM55_CTL={cm55_ctl_addr:#010x}",
        );

        self.power_on_cm55_domain(interface, pd_sense_addr)?;
        self.enable_cm55_ap(interface, ap_ctl_addr)?;
        self.prepare_cm55_core(interface, cm55_ctl_addr)?;
        self.cm55_enabled.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn power_on_cm55_domain(
        &self,
        interface: &mut dyn ArmDebugInterface,
        address: u64,
    ) -> Result<(), ArmError> {
        const MAX_ATTEMPTS: usize = 2;
        let mut read_error = None;
        let mut value = None;

        for attempt in 1..=MAX_ATTEMPTS {
            let read_result = {
                let mut cm33_ap = cm33_memory_interface(interface, &self.cm33_ap)?;
                cm33_ap.read_word_32(address)
            };

            match read_result {
                Ok(raw_value) => {
                    let mut sense = PwrmodePd6PdSense(raw_value);
                    if !sense.pd_on_cm55() {
                        tracing::debug!("Powering on CM55 domain (PD6.PD_SENSE @ {address:#010x})");
                        sense.set_pd_on_cm55(true);
                        value = Some(sense.0);
                    }
                    read_error = None;
                    break;
                }
                Err(error) => {
                    tracing::debug!(
                        "PSOC Edge: PD_SENSE read at {address:#010x} failed (attempt {attempt}/{MAX_ATTEMPTS}); recovering DP: {error:?}"
                    );
                    // Clear sticky errors on every failure, including the last, so a
                    // disabled CM55 never leaves the shared DP degraded for CM33.
                    common::recover_dp(interface, self.cm33_ap.dp());
                    read_error = Some(error);
                }
            }
        }

        if let Some(error) = read_error {
            return Err(error);
        }

        if let Some(value) = value {
            self.write_cm33_control_with_recovery(interface, address, value, "PD_SENSE")?;
        }
        Ok(())
    }

    fn enable_cm55_ap(
        &self,
        interface: &mut dyn ArmDebugInterface,
        address: u64,
    ) -> Result<(), ArmError> {
        // AP_CTL's secure alias is safe, but an access can still be rejected by the
        // current CM33 security state. Retry the known non-secure alias after clearing
        // transport sticky state; never apply this fallback to PD_SENSE, whose secure
        // write is target-poisoning even when the transport reports a recoverable error.
        let (control, address) = {
            let primary_result = {
                let mut cm33_ap = cm33_memory_interface(interface, &self.cm33_ap)?;
                cm33_ap.read_word_32(address)
            };

            match primary_result {
                Ok(value) => (AppCpussApCtl(value), address),
                Err(primary_error) if address != AppCpussApCtl::ADDRESS => {
                    tracing::debug!(
                        "PSOC Edge: AP_CTL read failed at {address:#010x}; retrying non-secure alias: {primary_error:?}"
                    );
                    common::recover_dp(interface, self.cm33_ap.dp());

                    let fallback_result = {
                        let mut cm33_ap = cm33_memory_interface(interface, &self.cm33_ap)?;
                        cm33_ap.read_word_32(AppCpussApCtl::ADDRESS)
                    };
                    (AppCpussApCtl(fallback_result?), AppCpussApCtl::ADDRESS)
                }
                Err(error) => return Err(error),
            }
        };

        let value = {
            let mut control = control;
            if control.cm55_enable() && control.cm55_dbg_enable() && control.cm55_nid_enable() {
                None
            } else {
                control.set_cm55_enable(true);
                control.set_cm55_dbg_enable(true);
                control.set_cm55_nid_enable(true);
                Some(control.0)
            }
        };
        if let Some(value) = value {
            self.write_cm33_control_with_recovery(interface, address, value, "AP_CTL")?;
        }
        Ok(())
    }

    fn prepare_cm55_core(
        &self,
        interface: &mut dyn ArmDebugInterface,
        cm55_ctl_addr: u64,
    ) -> Result<(), ArmError> {
        let mut halted = false;
        {
            let mut cm55_ap = interface.memory_interface(&self.cm55_ap)?;
            let mut dhcsr = Dhcsr(0);
            dhcsr.enable_write();
            dhcsr.set_c_debugen(true);
            dhcsr.set_c_halt(true);
            cm55_ap.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

            const MAX_RETRIES: usize = 10;
            for attempt in 1..=MAX_RETRIES {
                let dhcsr = Dhcsr(cm55_ap.read_word_32(Dhcsr::get_mmio_address())?);
                if dhcsr.c_debugen() && dhcsr.s_halt() {
                    tracing::debug!(
                        "CM55 debug interface enabled and core halted (attempt {attempt}/{MAX_RETRIES})"
                    );
                    halted = true;
                    break;
                }
                if attempt < MAX_RETRIES {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        }

        let mut wait_read_faulted = false;
        let cpu_wait = {
            let mut cm33_ap = cm33_memory_interface(interface, &self.cm33_ap)?;
            match cm33_ap.read_word_32(cm55_ctl_addr) {
                Ok(value) => MxCm55Ctl(value).cm55_wait(),
                Err(error) => {
                    tracing::debug!(
                        "CM55 wait-state register not readable ({error:?}); assuming firmware manages the CM55"
                    );
                    wait_read_faulted = true;
                    false
                }
            }
        };
        if wait_read_faulted {
            common::recover_dp(interface, self.cm33_ap.dp());
        }

        if cpu_wait {
            tracing::debug!("CM55 held in CPU_WAIT; releasing it without changing PC/SP");
            self.release_cm55_from_wait(interface, cm55_ctl_addr)?;
        } else if !halted {
            self.ensure_cm55_halted(interface)?;
        }
        Ok(())
    }

    /// Lightweight re-attach path for an already-enabled CM55.
    ///
    /// Instead of re-running the full power-up / CPU_WAIT-release sequence (which
    /// re-writes DHCSR and can resume a core the debugger had already halted), this
    /// just confirms the CM55 debug interface is still responding and (re-)halts the
    /// core for the debugger.
    ///
    /// Returns `Err(ArmError::CoreDisabled)` if the CM55 debug interface is no longer
    /// accessible, so the caller can fall back to a full re-enable.
    fn ensure_cm55_halted(&self, interface: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        let mut cm55_ap = interface.memory_interface(&self.cm55_ap)?;

        let dhcsr = Dhcsr(cm55_ap.read_word_32(Dhcsr::get_mmio_address())?);
        tracing::debug!("PSOC Edge: DHCSR = 0x{:08X}", dhcsr.0);
        drop(cm55_ap);
        if !dhcsr.c_debugen() {
            return Err(ArmError::CoreDisabled);
        }
        if dhcsr.s_halt() {
            // Already halted — nothing to do.
            tracing::debug!("PSOC Edge: CM55 core already halted");
            return Ok(());
        }

        // Core is running (e.g. firmware released it after the previous attach); halt
        // it so the debugger can read its state.
        let mut cm55_ap = interface.memory_interface(&self.cm55_ap)?;
        let mut halt = Dhcsr(0);
        halt.enable_write();
        halt.set_c_debugen(true);
        halt.set_c_halt(true);
        cm55_ap.write_word_32(Dhcsr::get_mmio_address(), halt.into())?;

        let mut dhcsr = Dhcsr(0);
        for _ in 0..100 {
            dhcsr = Dhcsr(cm55_ap.read_word_32(Dhcsr::get_mmio_address())?);
            if dhcsr.s_halt() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        if dhcsr.s_halt() {
            tracing::debug!("PSOC Edge: CM55 core halted successfully");
        } else if dhcsr.s_sleep() {
            // The AHB-AP still answers for SCS, so registers read fine while the core's
            // TCM and power-gated SRAM stay unreachable.
            tracing::warn!(
                "PSOC Edge: CM55 is in deep sleep and did not respond to the halt request \
                 (DHCSR={:#010x}); its TCM and power-gated SRAM are unreachable until the \
                 application wakes the core, so memory accesses will fail even though \
                 register access works",
                dhcsr.0
            );
        } else {
            tracing::warn!(
                "PSOC Edge: CM55 did not halt within 100ms (DHCSR={:#010x})",
                dhcsr.0
            );
        }
        Ok(())
    }

    /// Release the CM55 from its post-reset wait state.
    ///
    /// Sets vector-catch-on-reset so the core halts the instant it is released, clears
    /// the `CPU_WAIT` bit via the CM33 AP, halts the core, restores the caller's
    /// original `DEMCR`, then pre-initializes the core to a safe state.
    fn release_cm55_from_wait(
        &self,
        interface: &mut dyn ArmDebugInterface,
        cm55_ctl_addr: u64,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::armv8m::Demcr;
        tracing::debug!("PSOC Edge: Releasing CM55 from wait state");
        // Enable vector-catch-on-reset so the CM55 halts immediately once released.
        let saved_demcr = {
            let mut cm55_ap = interface.memory_interface(&self.cm55_ap)?;
            let saved = cm55_ap.read_word_32(Demcr::get_mmio_address())?;
            let mut demcr = Demcr(saved);
            demcr.set_vc_corereset(true);
            cm55_ap.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
            saved
        };

        // Clear CPU_WAIT via the CM33 AP so the CM55 leaves its wait state.
        {
            let mut cm33_ap = cm33_memory_interface(interface, &self.cm33_ap)?;
            let mut ctl = MxCm55Ctl(cm33_ap.read_word_32(cm55_ctl_addr)?);
            ctl.set_cm55_wait(false);
            cm33_ap.write_word_32(cm55_ctl_addr, ctl.0)?;
        }

        // Halt the CM55 and wait until it reports being in debug state.
        let halted = {
            let mut cm55_ap = interface.memory_interface(&self.cm55_ap)?;
            let mut dhcsr = Dhcsr(0);
            dhcsr.enable_write();
            dhcsr.set_c_debugen(true);
            dhcsr.set_c_halt(true);
            cm55_ap.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

            let mut halted = false;
            for _ in 0..100 {
                if Dhcsr(cm55_ap.read_word_32(Dhcsr::get_mmio_address())?).s_halt() {
                    halted = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            halted
        };

        // Restore the original DEMCR (removes the temporary vector-catch).
        {
            let mut cm55_ap = interface.memory_interface(&self.cm55_ap)?;
            cm55_ap.write_word_32(Demcr::get_mmio_address(), saved_demcr)?;
        }

        if !halted {
            tracing::warn!(
                "CM55 did not halt after clearing CPU_WAIT; skipping pre-initialization"
            );
            return Ok(());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{PsocEdge, cm33_security_policy};
    use crate::architecture::arm::sequences::ArmDebugSequence;
    use crate::config::Registry;

    #[test]
    fn cm33_security_policy_selects_alias_security() {
        assert!(!cm33_security_policy(0x5000_0000));
        assert!(cm33_security_policy(0x4241_0060));
        assert!(!cm33_security_policy(0x5241_0060));
        assert!(cm33_security_policy(0x4416_0000));
        assert!(!cm33_security_policy(0x5416_0000));
    }

    /// HNONSEC=1 on 0x24006000 drops the CM33 AP on a booted PSE846.
    #[test]
    fn cm33_security_policy_keeps_sram_secure() {
        for addr in [0x2400_6000, 0x2408_3000, 0x3400_6000, 0x340F_FFFC] {
            assert!(
                !cm33_security_policy(addr),
                "{addr:#010x} must use HNONSEC=0"
            );
        }
    }

    #[test]
    fn memory_security_policy_applies_to_cm33_ap_only() {
        let registry = Registry::from_builtin_families();
        let family = registry
            .families()
            .iter()
            .find(|family| family.name == "psoc_e84")
            .unwrap();
        let chip = family.variants().first().unwrap();
        let sequence = PsocEdge::create(chip);

        assert!(
            sequence
                .memory_security_policy(&sequence.cm33_ap)
                .is_some_and(|policy| policy(0x4241_0060) && !policy(0x5241_0060))
        );
        assert!(sequence.memory_security_policy(&sequence.cm55_ap).is_none());
    }

    #[test]
    fn ppc_find_region_matches_first_sized_region() {
        // R_ATT[28:24] = size exponent n, region spans 2 << n bytes.
        let r_addr = [0x4400_0000, 0x441C_0000, 0x4416_0000, 0x441C_1000];
        let r_att = [0, 11 << 24, 15 << 24, 11 << 24];

        assert_eq!(
            PsocEdge::ppc_find_region(0x441C_1000, &r_addr, &r_att),
            Some(3)
        );
        assert_eq!(
            PsocEdge::ppc_find_region(0x4416_0004, &r_addr, &r_att),
            Some(2)
        );
        assert_eq!(
            PsocEdge::ppc_find_region(0x4400_0000, &r_addr, &r_att),
            None
        );
        assert_eq!(
            PsocEdge::ppc_find_region(0x4500_0000, &r_addr, &r_att),
            None
        );
    }

    #[test]
    fn validate_psoc_edge_targets() {
        let registry = Registry::from_builtin_families();
        let family = registry
            .families()
            .iter()
            .find(|family| family.name == "psoc_e84")
            .unwrap();
        for chip in family.variants() {
            _ = PsocEdge::create(chip);
        }
    }
}
