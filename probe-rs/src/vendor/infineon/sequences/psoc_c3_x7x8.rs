//! Debug sequences for PSOC C3 x7/x8 (CAT1B P7/P8/M7/M8) devices.
//!
//! The DP (ADIv6 DPv3 MinDP, `debugconfig dormant="1"`) starts in dormant state and
//! only responds after the DORMANT-to-SWD selection alert sequence (ARM ADI §B4.3.4).
//! A failed DPIDR read returns the DP to dormant, so the full sequence must be re-sent
//! before every attempt. The shared PSC3 setup helper owns this wake-up and reset
//! recovery so the x7/x8 adapter can remain focused on its additional policy.
//!
//! # PPCA core support (x7/x8 variants)
//!
//! x7 devices contain one extra Cortex-M33 PPCA core; x8 devices contain two. Each
//! PPCA core lives in its own power domain and must be initialised before the debug
//! access port for that core becomes functional. `PpcaController::acquire` performs this
//! initialisation:
//!
//! 1. Powers the PPCA peripheral group (`PERI0_GR4_SL_CTL`).
//! 2. Writes a minimal stub (initial MSP + Reset_Handler pointing at an endless
//!    `B .` loop) into the core's SRAM so the core executes something sensible.
//! 3. Releases the core from reset (`CNFG_CPU_CTRL`, `CNFG_RST_CTRL`,
//!    `MXCM33x_CM33_CTL`).
//! 4. Opens the PPCA Access Ports (`PPCA_CPUSS_AP_CTL`, `CPUSS_AP_CTL`).
//!
//! The sequence runs inside `on_attach`, which is called on every `session.core()`
//! access. The idempotency check in `PpcaController::acquire` makes repeated calls safe:
//! cold-start initialisation runs once; subsequent accesses are no-ops.

use std::{sync::Arc, thread, time::Duration};

use bitfield::bitfield;
use probe_rs_target::Chip;

use crate::{
    Permissions,
    architecture::arm::{
        ArmDebugInterface, ArmError, DapAccess, FullyQualifiedApAddress,
        dp::{Ctrl, DpAccess, DpAddress},
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, DebugEraseSequence, DefaultArmSequence},
        traits::DebugPortWire,
    },
    probe::WireProtocol,
};

use super::common::AP_CSW;
use super::{
    common,
    psoc_c3_common::{self, Cm33ApCsw, SysApCsw},
    psoc_c3_erase::DualBankErase,
    psoc_c3_ppca::PpcaController,
};

/// RAM address where the debug certificate is loaded (mandatory for x7/x8 series).
const DEBUG_CERTIFICATE: u32 = 0x3400_4000;

/// BootROM status register used to identify the current debug-acquisition state.
const BOOT_STATUS_ADDRESS: u32 = 0x5220_0418;

bitfield! {
    /// SRSS soft reset control register payload.
    #[derive(Clone, Copy)]
    struct SrssResSoftCtl(u32);
    impl Debug;

    pub trig_soft, set_trig_soft: 0;
}

impl SrssResSoftCtl {
    const ADDRESS: u32 = 0x4220_0410;

    fn soft_reset_request() -> Self {
        let mut v = Self(0);
        v.set_trig_soft(true);
        v
    }
}

/// SRSS Boot DLM Control register — debug unlock mode request.
struct SrssBootDlmCtl;

impl SrssBootDlmCtl {
    const ADDRESS: u32 = 0x4220_0404;
    /// OEM_ROT_KEY_SIGNED: request WFA mode with a debug certificate.
    const WFA_REQUEST_DEBUG_CERT: u32 = 2;
}

/// SRSS Boot DLM Control 2 register — holds the debug certificate RAM address.
struct SrssBootDlmCtl2;

impl SrssBootDlmCtl2 {
    const ADDRESS: u32 = 0x4220_0408;
}

/// OR with a register address to use the TrustZone secure alias (`0x4220_xxxx` → `0x5220_xxxx`).
const SECURE_ALIAS_OFFSET: u32 = 0x1000_0000;

/// Time required for the BootROM to reopen the debug interface after reset.
const RESET_DELAY_MS: u64 = common::RESET_FINISH_DELAY_MS;

/// PSOC C3 x7/x8 (CAT1B P7/P8/M7/M8) debug sequences.
#[derive(Debug)]
pub struct PsocC3X7X8 {
    /// The CM33 access port (primary core, used to initialise PPCA SRAM).
    cm33_ap: FullyQualifiedApAddress,
    /// PPCA access ports, in core-index order: index 0 = PPCA Core0, index 1 = PPCA Core1.
    ppca: PpcaController,
    /// How this part has to be erased, decided once the device reports its bank layout.
    erase: DualBankErase,
}

impl PsocC3X7X8 {
    /// Creates debug sequences for a PSOC C3 x7/x8 chip.
    pub fn create(chip: &Chip) -> Arc<Self> {
        let dp = DpAddress::Default;

        // The CM33 AP is always at base 0xF000_2000.
        let cm33_ap = Self::cm33_ap(dp);

        Arc::new(PsocC3X7X8 {
            cm33_ap,
            ppca: PpcaController::new(chip),
            erase: DualBankErase::new(chip),
        })
    }

    fn cm33_ap(dp: DpAddress) -> FullyQualifiedApAddress {
        psoc_c3_common::cm33_ap(dp)
    }
}

impl ArmDebugSequence for PsocC3X7X8 {
    /// Called before attaching to a core's AP.
    ///
    /// For PPCA cores this runs `PpcaController::acquire` to power the domain, load the
    /// stub, start the core, and open the debug AP before any register access.
    /// Unlike PSOC Edge's CM55 (gated by firmware via `CM55_WAIT`), PPCA acquisition
    /// fully owns bring-up every time — there is no "not ready yet" state, so
    /// failures are propagated as-is rather than mapped to `ArmError::CoreDisabled`.
    fn on_attach(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        core_type: probe_rs_target::CoreType,
    ) -> Result<(), ArmError> {
        if let Some(core_index) = self.ppca.core_index(core_ap) {
            self.ppca
                .acquire(interface, &self.cm33_ap, core_index, false)
                .inspect_err(|e| {
                    tracing::warn!(
                        "PSOC C3 x7/x8: PPCA acquisition for Core{core_index} failed: {e}"
                    );
                })
        } else {
            DefaultArmSequence(()).on_attach(interface, core_ap, core_type)
        }
    }

    /// Erase flash through the SROM API, but only on a dual-bank device.
    fn debug_erase_sequence(&self) -> Option<Arc<dyn DebugEraseSequence>> {
        self.erase.sequence(self.cm33_ap.clone())
    }

    /// `--connect-under-reset` is not supported on PSOC C3 x7/x8 devices.
    ///
    /// The debug port starts in dormant state and requires the chip to be powered
    /// and running before the DORMANT-to-SWD alert sequence can be sent.  Asserting
    /// nRESET before connecting would prevent `debug_port_setup` from waking the DP,
    /// so this override makes `--connect-under-reset` a no-op and emits a warning.
    fn reset_hardware_assert(&self, _interface: &mut dyn DebugPortWire) -> Result<(), ArmError> {
        tracing::warn!(
            "PSOC C3 x7/x8: `--connect-under-reset` is not supported — \
             the debug port requires the chip to be running before the \
             dormant wake-up sequence can succeed. \
             The nRESET assertion is skipped; connect without reset instead."
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

    /// For PPCA cores `debug_core_start` is a no-op: the core is initialised and
    /// its debug AP opened inside `on_attach`/`PpcaController::acquire`.
    fn debug_core_start(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        core_type: probe_rs_target::CoreType,
        debug_base: Option<u64>,
        cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        if self.ppca.is_ap(core_ap) {
            // PPCA AP initialisation is done in on_attach(); nothing more needed here.
            return Ok(());
        }
        DefaultArmSequence(()).debug_core_start(interface, core_ap, core_type, debug_base, cti_base)
    }

    fn debug_port_setup(
        &self,
        interface: &mut dyn DebugPortWire,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        if interface.active_protocol() == Some(WireProtocol::Jtag) {
            tracing::debug!("PSOC C3 x7/x8: JTAG — dormant-to-JTAG wake with XRES reset-acquire");
            return psoc_c3_common::debug_port_setup_dormant_jtag(interface, dp);
        }

        psoc_c3_common::debug_port_setup(interface, dp)
    }

    /// Powers up the debug port per the CAT1 programming spec, split by protocol.
    ///
    /// - JTAG: post `CTRL/STAT` (`CSYSPWRUPREQ | CDBGPWRUPREQ` plus the write-1-to-clear
    ///   sticky flags), priming the power-up before the stock `DPIDR` read since the
    ///   SWJ-DP NACKs DPACC reads until the DAP is powered up.
    /// - SWD: clear sticky errors via `ABORT` (read-only in CTRL/STAT here), then run the
    ///   stock power-up.
    fn debug_port_start(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        psoc_c3_common::debug_port_start(interface, dp)
    }

    fn debug_port_stop(
        &self,
        interface: &mut dyn DebugPortWire,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        psoc_c3_common::debug_port_stop(interface, dp)
    }

    /// Checks `DeviceEn` in the CM33 AP CSW. If clear, sends a WFA debug-cert
    /// request to the boot ROM, triggers a soft reset, and returns
    /// [`ArmError::ReAttachRequired`]. If set, configures `HNONSEC` from `SDeviceEn`.
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &Permissions,
    ) -> Result<(), ArmError> {
        let dp = default_ap.dp();
        let cm33_ap = Self::cm33_ap(dp);

        // Flush any pending operations before reading the CSW, then give the
        // boot ROM a short settling window to stabilise DeviceEn.
        let _ = interface.flush();
        thread::sleep(Duration::from_millis(10));

        let csw = interface.read_raw_ap_register(&cm33_ap, AP_CSW)?;
        tracing::debug!("PSOC C3 x7/x8: CM33 AP CSW = 0x{:08X}", csw);

        let csw_bits = Cm33ApCsw(csw);

        if !csw_bits.device_en() {
            tracing::warn!(
                "PSOC C3 x7/x8: DeviceEn=0 (AP closed) — sending WFA request and resetting"
            );

            let sys_ap = common::sys_ap(dp);
            interface.write_raw_ap_register(&sys_ap, AP_CSW, SysApCsw::secure_word().0)?;

            // CTRL/STAT.READOK=1 means the previous AP read was in the secure domain.
            let ctrl_stat: Ctrl = interface.read_dp_register(dp)?;
            let sec_off: u32 = if ctrl_stat.read_ok() {
                SECURE_ALIAS_OFFSET
            } else {
                0
            };
            tracing::debug!("PSOC C3 x7/x8: domain_secure={}", ctrl_stat.read_ok());

            // Write cert address first — mandatory for x7/x8 series.
            common::write_mem32(
                interface,
                &sys_ap,
                SrssBootDlmCtl2::ADDRESS | sec_off,
                DEBUG_CERTIFICATE,
            )?;

            // Set WFA request; boot ROM reads it after soft reset.
            common::write_mem32(
                interface,
                &sys_ap,
                SrssBootDlmCtl::ADDRESS | sec_off,
                SrssBootDlmCtl::WFA_REQUEST_DEBUG_CERT,
            )?;

            // Trigger soft reset via SYS AP — SWD will drop, error is expected.
            let _ = common::write_mem32(
                interface,
                &sys_ap,
                SrssResSoftCtl::ADDRESS | sec_off,
                SrssResSoftCtl::soft_reset_request().0,
            );
            // Flush the CMSIS-DAP batch so the DRW write reaches the chip before we sleep.
            let _ = interface.flush();

            tracing::debug!(
                "PSOC C3 x7/x8: waiting {}ms for boot ROM to process WFA request",
                RESET_DELAY_MS
            );
            thread::sleep(Duration::from_millis(RESET_DELAY_MS));

            // Signal probe-rs to re-do dormant wake-up + DP power-up.
            return Err(ArmError::ReAttachRequired);
        }

        // Diagnostic only: record the BootROM state without changing acquisition state.
        let sys_ap = common::sys_ap(dp);
        interface.write_raw_ap_register(&sys_ap, AP_CSW, SysApCsw::secure_word().0)?;
        interface.write_raw_ap_register(&sys_ap, common::AP_TAR, BOOT_STATUS_ADDRESS)?;
        let boot_status = interface.read_raw_ap_register(&sys_ap, common::AP_DRW);
        tracing::debug!(
            "PSOC C3 x7/x8: BootROM BOOT_STATUS @ 0x{BOOT_STATUS_ADDRESS:08X} = {:?}",
            boot_status
        );

        // AP is open. Set HNONSEC based on SDeviceEn and restore all required
        // functional fields (Size=Word, AddrInc=Single, HPROT[0,1,3]).
        // probe-rs's amba_ahb3/5 also does this on AP init, but doing it explicitly
        // here ensures the hardware CSW is correct before the AP adapter reads it.
        let new_csw = Cm33ApCsw::with_standard_access(csw);

        tracing::debug!(
            "PSOC C3 x7/x8: CM33 AP CSW 0x{:08X} -> 0x{:08X} (SDeviceEn={}, HNONSEC={})",
            csw,
            new_csw.0,
            csw_bits.s_device_en(),
            new_csw.hnonsec(),
        );
        interface.write_raw_ap_register(&cm33_ap, AP_CSW, new_csw.0)?;

        // Detect and log the flash bank mode (single vs dual). Best-effort - a read
        // failure here must not abort attach, so the error is intentionally ignored.
        if let Ok(mode) = psoc_c3_common::detect_flash_bank_mode(interface, &cm33_ap) {
            self.erase.set_mode(mode);
        }

        Ok(())
    }

    /// Resets the SoC via `SRSS_RES_SOFT_CTL` through the SYS AP.
    ///
    /// `SRSS_RES_SOFT_CTL` through the SYS AP works regardless of CM33 AP state.
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // PPCA cores are reset as part of the whole-SoC reset that happens when the
        // CM33 core is reset via SRSS_RES_SOFT_CTL.  They have no independent reset
        // mechanism, and their APs may be inaccessible after a SoC reset.
        // Returning Ok(()) here avoids triggering a second SoC reset and prevents
        // cortex_m_wait_for_reset from polling a powered-down PPCA AP (→ timeout).
        if self.ppca.is_ap(&interface.fully_qualified_address()) {
            tracing::debug!(
                "PSOC C3 x7/x8: reset_system — skipping for PPCA AP {:?} (reset handled by CM33 reset_system)",
                interface.fully_qualified_address()
            );
            return Ok(());
        }

        psoc_c3_common::halt_and_soft_reset(interface, Duration::from_millis(RESET_DELAY_MS))?;
        let arm = interface.get_arm_debug_interface()?;
        psoc_c3_common::apply_standard_cm33_csw(arm, &self.cm33_ap)?;

        // Re-acquire every PPCA core: re-power the domain, re-write the stub,
        // re-open the AP (PPCA_CPUSS_AP_CTL and CPUSS_AP_CTL are cleared by SRSS
        // soft reset), and halt via DHCSR.  CPU_CTRL / RST_CTRL are left at their
        // reset defaults (0x3) so the CM33 BSP skips PPCA IPC initialisation.
        {
            let arm = interface.get_arm_debug_interface()?;
            for (core_index, _) in self.ppca.iter().enumerate() {
                tracing::debug!("PSOC C3 x7/x8: reset_system — re-acquiring PPCA Core{core_index}");
                if let Err(e) = self.ppca.acquire(arm, &self.cm33_ap, core_index, true) {
                    tracing::warn!(
                        "PSOC C3 x7/x8: reset_system — PPCA acquisition for Core{core_index} failed: {e}"
                    );
                }
            }
        }

        Ok(())
    }
}
