//! Debug sequences for PSOC C3 (M3/M5/P2/P5) and PSOC C3 x6 (M6/P6) devices.
//!
//! These devices' CM33 access port is a TrustZone-aware AHB5 AP whose `CSW.HNONSEC`
//! bit must be derived from `CSW.SDeviceEn` (secure debug enabled). The generic AHB5
//! AP adapter does not know about this Infineon-specific convention, so without this
//! correction AP register writes (e.g. `DRW`, used to halt the CPU via `DHCSR` before
//! flashing) fail with "Target device did not respond to request".
//!
//! If the AP is closed (`CSW.DeviceEn == 0`), the vendor unlock flow uploads a signed
//! debug certificate and requests a WFA ("wait for authentication") unlock — this
//! requires a certificate file from the host toolchain that probe-rs has no
//! infrastructure for yet, so it is not implemented here. If `DeviceEn` is closed,
//! this sequence reports the condition clearly instead of silently failing on the
//! next AP register access.
//!
//! The M3/M5/P2/P5 and M6/P6 families are otherwise identical; the only difference is
//! that x6 (M6/P6) latches the DP sticky-error flags on the intentional soft-reset
//! transaction fault and must clear them after reset. This is selected via
//! [`PsocC3::create_x6`].

use std::{sync::Arc, time::Duration};

use probe_rs_target::Chip;

use crate::{
    Permissions,
    architecture::arm::{
        ArmDebugInterface, ArmError, DapAccess, FullyQualifiedApAddress,
        dp::DpAddress,
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, DebugEraseSequence, DefaultArmSequence},
        traits::DebugPortWire,
    },
};

use super::common;
use super::{psoc_c3_common, psoc_c3_erase::DualBankErase};

/// Post-reset settle time — the boot ROM needs this long to re-open the AP after a
/// soft reset. Use the shared Infineon boot-window budget; the extra margin is
/// important on PSC3M5 parts where the AP can reappear after the first reconnect.
const RESET_DELAY_MS: u64 = common::RESET_FINISH_DELAY_MS;

/// Generic PSOC C3 debug sequence for devices without a custom debug-cert
/// unlock flow (M3/M5/P2/P5 and M6/P6).
#[derive(Debug)]
pub struct PsocC3 {
    /// x6 (M6/P6) latches the DP sticky-error flags on the intentional soft-reset
    /// transaction fault; clear them before touching the AP again after reset.
    clear_sticky_errors_after_reset: bool,
    /// How this part has to be erased, decided once the device reports its bank layout.
    erase: DualBankErase,
}

impl PsocC3 {
    /// Creates the debug sequence for a generic PSOC C3 chip (M3/M5/P2/P5).
    pub fn create(chip: &Chip) -> Arc<Self> {
        Arc::new(PsocC3 {
            clear_sticky_errors_after_reset: false,
            erase: DualBankErase::new(chip),
        })
    }

    /// Creates the debug sequence for a PSOC C3 x6 (M6/P6) chip.
    ///
    /// Behaves like [`create`](Self::create) but additionally clears the DP
    /// sticky-error flags latched by the intentional soft-reset transaction fault,
    /// so that post-reset AP accesses (the DRW writes that halt the core and clear
    /// the reset vector catch) do not fail.
    pub fn create_x6(chip: &Chip) -> Arc<Self> {
        Arc::new(PsocC3 {
            clear_sticky_errors_after_reset: true,
            erase: DualBankErase::new(chip),
        })
    }
}

impl ArmDebugSequence for PsocC3 {
    /// `--connect-under-reset` is not supported on PSOC C3 devices.
    ///
    /// The debug port starts dormant and the boot ROM must be running to reopen
    /// the SWJ-DP. Holding nRESET asserted prevents the dormant wake sequence
    /// from seeing a TAP/AP, so let the normal vendor reset/reconnect flow run.
    fn reset_hardware_assert(&self, _interface: &mut dyn DebugPortWire) -> Result<(), ArmError> {
        tracing::warn!(
            "PSOC C3: `--connect-under-reset` is not supported; skipping nRESET assertion"
        );
        Ok(())
    }

    /// Matching no-op for [`reset_hardware_assert`](Self::reset_hardware_assert).
    fn reset_hardware_deassert(
        &self,
        _interface: &mut dyn ArmDebugInterface,
        _default_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        Ok(())
    }

    /// Wakes the dormant SWJ-DP into JTAG before the TAP scan.
    ///
    /// The PSOC C3 SWJ-DP powers up in the ARM dormant state, so the default
    /// SWD-to-JTAG switch never selects JTAG and the TAP scan finds 0 TAPs. Send the
    /// DORMANT-to-JTAG selection alert first (see
    /// `psoc_c3_common::debug_port_setup_dormant_jtag`).
    /// SWD is unaffected and uses the default sequence.
    fn debug_port_setup(
        &self,
        interface: &mut dyn DebugPortWire,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        psoc_c3_common::debug_port_setup(interface, dp)
    }

    /// Powers up the debug port per the CAT1 programming spec, split by protocol.
    ///
    /// - JTAG: post `CTRL/STAT = 0x5000_0032` (`CSYSPWRUPREQ | CDBGPWRUPREQ` plus the
    ///   write-1-to-clear sticky flags). Over JTAG the SWJ-DP NACKs the default
    ///   sequence's initial `DPIDR` read until the DAP is powered up, so on the first
    ///   failure we post the power-up write and retry (`prime_before_first`).
    /// - SWD: clear sticky errors via `ABORT` (they are read-only in CTRL/STAT here),
    ///   then run the stock power-up.
    fn debug_port_start(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        // JTAG-only: the SWJ-DP NACKs DPACC reads (including the DPIDR read the default
        // sequence does first) until the DAP is explicitly powered up, and the first posted
        // DPACC scan reads a stale ACK until the pipeline is primed. Prime the ABORT + JTAG
        // CTRL/STAT power-up write up front so the default sequence's DPIDR read succeeds.
        // On SWD this branches to the ABORT-based sticky-error clear instead.
        common::dp_start_with_powerup(interface, dp, true, true)
    }

    /// Corrects `CSW.HNONSEC` based on `CSW.SDeviceEn` on the default AP.
    ///
    /// Without this, AP register writes (e.g. `DRW`, used to write `DHCSR` when
    /// halting the CPU before flashing) fail with no response, because the generic
    /// AHB5 AP adapter does not apply Infineon's secure-debug-derived HNONSEC
    /// convention.
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &Permissions,
    ) -> Result<(), ArmError> {
        psoc_c3_common::apply_standard_cm33_csw(interface, default_ap)?;

        // Record the flash bank layout for the erase path. Best-effort - a read failure here
        // must not abort attach, it only means a dual-bank part is treated as single-bank.
        if let Ok(mode) = psoc_c3_common::detect_flash_bank_mode(interface, default_ap) {
            self.erase.set_mode(mode);
        }

        Ok(())
    }

    fn debug_erase_sequence(&self) -> Option<Arc<dyn DebugEraseSequence>> {
        self.erase
            .sequence(psoc_c3_common::cm33_ap(DpAddress::Default))
    }

    /// Resets the SoC via `SRSS_RES_SOFT_CTL` (through the SYS AP) and re-applies the
    /// CM33 AP CSW correction afterwards.
    ///
    /// The default AIRCR.SYSRESETREQ-based reset doesn't work on this device — the
    /// `SRSS_RES_SOFT_CTL` through the SYS AP works regardless of CM33 AP state.
    /// The SRSS soft reset also clears the CM33
    /// AP CSW (HNONSEC/SDeviceEn), so without re-applying it here, the very next AP
    /// register access (e.g. halting the core to run the flash algorithm) fails
    /// with "Target device did not respond to request".
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        psoc_c3_common::halt_and_soft_reset(interface, Duration::from_millis(RESET_DELAY_MS))?;

        let dp = interface.fully_qualified_address().dp();
        let cm33_ap = interface.fully_qualified_address();
        let arm = interface.get_arm_debug_interface()?;

        if self.clear_sticky_errors_after_reset {
            // reset_and_halt's next steps (the DRW writes that halt the core and clear the
            // reset vector catch) fault while the flags latched by the reset are still set.
            let _ = common::clear_dp_sticky_errors(arm, dp);
        }

        // SRSS soft reset clears the CM33 AP CSW — re-apply HNONSEC/access fields
        // so the very next AP register access doesn't fail.
        psoc_c3_common::apply_standard_cm33_csw(arm, &cm33_ap)
    }

    /// Clears the DP sticky-error flags before the stock core-stop writes.
    ///
    /// By the time the session disconnects, the firmware may have reset itself, entered
    /// deep sleep, or exited via semihosting — any of which faults the in-flight AP
    /// access and latches the DP sticky-error flags. While those are set, the stock
    /// `debug_core_stop` DHCSR/DEMCR writes fault ("FAULT response to the request") and
    /// abort the whole session teardown. Clear the sticky flags first so the
    /// disable-debug writes go through.
    fn debug_core_stop(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        core_type: probe_rs_target::CoreType,
    ) -> Result<(), ArmError> {
        let dp = interface.fully_qualified_address().dp();
        if let Ok(arm) = interface.get_arm_debug_interface()
            && let Err(e) = common::clear_dp_sticky_errors(arm, dp)
        {
            tracing::warn!("PSOC C3: failed to clear DP sticky errors before core stop: {e:?}");
        }

        self.debug_core_stop_default(interface, core_type)
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
}
