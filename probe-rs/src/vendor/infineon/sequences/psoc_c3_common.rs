//! Shared primitives for the Infineon PSOC C3 debug sequences.
//!
//! The `PsocC3` (M3/M5/P2/P5) and `PsocC3X7X8`
//! (P7/P8/M7/M8) sequences share the same proprietary SYS AP bus-access portal and
//! the same TrustZone-aware CM33 AP `CSW` convention (`HNONSEC` derived from
//! `SDeviceEn`). This module holds the register definitions and helpers common to
//! all of them so they are defined once.

use std::{thread, time::Duration};

use bitfield::bitfield;

use crate::{
    MemoryMappedRegister,
    architecture::arm::{
        ApV2Address, ArmDebugInterface, ArmError, FullyQualifiedApAddress,
        core::armv7m::Dhcsr,
        dp::{Abort, DpAddress, DpRegister},
        memory::ArmMemoryInterface,
        sequences::cortex_m_wait_for_reset,
    },
};

/// AP CSW register offset (ADIv6 APv2 layout).
pub(super) const AP_CSW: u64 = 0xD00;
/// AP TAR register offset (ADIv6 APv2 layout).
pub(super) const AP_TAR: u64 = 0xD04;
/// AP DRW register offset (ADIv6 APv2 layout).
pub(super) const AP_DRW: u64 = 0xD0C;

/// SYS AP base address (`__apid=0`) — Infineon's proprietary bus-access AP, used to
/// reach `SRSS` registers regardless of the CM33 AP's lock state.
pub(super) const SYS_AP_BASE: u64 = 0xF000_0000;

bitfield! {
    /// SYS AP CSW register — Infineon proprietary bus-access AP.
    ///
    /// Bit layout matches the standard AMBA AHB-AP CSW layout (ADI spec C2.2).
    #[derive(Clone, Copy)]
    pub(super) struct SysApCsw(u32);
    impl Debug;

    /// DbgSwEnable — must be 1 to enable DAP software access.
    pub dbg_sw_enable, set_dbg_sw_enable: 31;
    /// HNONSEC — 0 = secure transaction, 1 = non-secure.
    pub hnonsec, set_hnonsec: 30;
    /// MasterType — selects debug master ID on HMASTER signals.
    pub master_type, set_master_type: 29;
    /// HPROT[3] Cacheable.
    pub cacheable, set_cacheable: 27;
    /// HPROT[1] Privileged.
    pub privileged, set_privileged: 25;
    /// HPROT[0] Data.
    pub data, set_data: 24;
    /// Size[2:0] — access width: 0=byte, 1=halfword, 2=word.
    u8, size, set_size: 2, 0;
}

impl SysApCsw {
    /// Standard value for SRSS writes:
    /// DbgSwEnable=1, HNONSEC=0, MasterType=1, Cacheable=1, Privileged=1, Data=1, Size=Word.
    pub(super) fn secure_word() -> Self {
        let mut v = Self(0);
        v.set_dbg_sw_enable(true);
        v.set_hnonsec(false); // secure
        v.set_master_type(true);
        v.set_cacheable(true);
        v.set_privileged(true);
        v.set_data(true);
        v.set_size(2); // Word
        v
    }
}

bitfield! {
    /// CM33 AP CSW register (TrustZone-aware AHB5 AP).
    #[derive(Clone, Copy)]
    pub(super) struct Cm33ApCsw(u32);
    impl Debug;

    /// CSW.Size\[2:0\] — access size: 0=byte, 1=halfword, 2=word.
    u8, size, set_size: 2, 0;
    /// CSW.AddrInc\[5:4\] — address increment: 0=off, 1=single, 2=packed.
    u8, addr_inc, set_addr_inc: 5, 4;
    /// CSW.DeviceEn bit — AP is open when set.
    pub device_en, _: 6;
    /// CSW.SDeviceEn/SPIDEN — secure debug enabled when set.
    pub s_device_en, _: 23;
    /// CSW.Data (HPROT\[0\]) — data access.
    pub data, set_data: 24;
    /// CSW.Privileged (HPROT\[1\]) — privileged access.
    pub privileged, set_privileged: 25;
    /// CSW.Cacheable (HPROT\[3\]) — cacheable access.
    pub cacheable, set_cacheable: 27;
    /// CSW.HNONSEC bit — non-secure transaction when set.
    pub hnonsec, set_hnonsec: 30;
}

impl Cm33ApCsw {
    /// Bits preserved from the original CSW (implementation-defined/reserved).
    /// Clears Size[2:0], AddrInc[5:4], and HPROT[0,1,3] for explicit reconfiguration.
    const PRESERVE_MASK: u32 = 0xB0FF_FFC0;

    /// Build the standard CSW for word-width privileged debug access.
    ///
    /// Preserves implementation-defined bits, then explicitly sets Size=Word,
    /// AddrInc=Single, HPROT\[0,1,3\], and HNONSEC from SDeviceEn.
    pub(super) fn with_standard_access(csw: u32) -> Self {
        let mut out = Self(csw & Self::PRESERVE_MASK);
        out.set_size(2); // Word (32-bit)
        out.set_addr_inc(1); // Single increment
        out.set_data(true); // HPROT[0]: data access
        out.set_privileged(true); // HPROT[1]: privileged
        out.set_cacheable(true); // HPROT[3]: cacheable
        // HNONSEC=0 when SDeviceEn=1 (secure debug active), =1 otherwise.
        out.set_hnonsec((csw >> 23) & 1 == 0);
        out
    }
}

/// The SYS AP for the given debug port.
pub(super) fn sys_ap(dp: DpAddress) -> FullyQualifiedApAddress {
    FullyQualifiedApAddress::v2_with_dp(dp, ApV2Address(Some(SYS_AP_BASE)))
}

/// Write one 32-bit word to `addr` through the given AP using raw TAR/DRW register
/// writes.
///
/// `memory_interface()` cannot be used for the SYS AP: that function initialises a
/// standard AMBA memory-AP adapter and resets CSW to a generic default, which clears
/// the SYS AP's `DbgSwEnable` bit and makes subsequent transfers fail. The SYS AP is
/// Infineon's proprietary bus-access portal, not a standard AHB/AXI AP.
pub(super) fn write_mem32(
    iface: &mut dyn ArmDebugInterface,
    ap: &FullyQualifiedApAddress,
    addr: u32,
    val: u32,
) -> Result<(), ArmError> {
    iface.write_raw_ap_register(ap, AP_TAR, addr)?;
    iface.write_raw_ap_register(ap, AP_DRW, val)?;
    Ok(())
}

/// Read one 32-bit word from `addr` through the given AP using raw TAR/DRW register
/// accesses.
///
/// See [`write_mem32`] for why `memory_interface()` cannot be used for the SYS AP.
pub(super) fn read_mem32(
    iface: &mut dyn ArmDebugInterface,
    ap: &FullyQualifiedApAddress,
    addr: u32,
) -> Result<u32, ArmError> {
    iface.write_raw_ap_register(ap, AP_TAR, addr)?;
    iface.read_raw_ap_register(ap, AP_DRW)
}

/// `FLASHC_FLASH_CTL` register — flash controller configuration (CM33 AP view).
///
/// Read to determine the flash bank mode. This mirrors Infineon's OpenOCD
/// `cat1b` flow, which reads the same register over the CM33 AP to decide whether
/// to expose a single main bank or split it into `main0`/`main1` dual banks.
pub(super) const FLASHC_FLASH_CTL: u64 = 0x5215_0000;
/// `FLASHC_FLASH_CTL.BANK` (bit 12) — set when the flash is in dual-bank mode.
pub(super) const FLASHC_FLASH_CTL_BANK: u32 = 0x0000_1000;

/// Physical flash bank layout as configured in `FLASHC_FLASH_CTL.BANK`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FlashBankMode {
    /// The main flash is a single contiguous bank (`main0` only).
    Single,
    /// The main flash is split into two equal banks (`main0` + `main1`),
    /// each mapped at its own S-bus / C-bus alias.
    Dual,
}

/// Read `FLASHC_FLASH_CTL.BANK` over the given (CM33) AP to detect whether the
/// device is running in single- or dual-bank flash mode, and log the result.
///
/// The bank layout is a provisioning choice, not a property of the part number, so it can
/// only be learned from the live device. The result selects the erase strategy: a dual-bank
/// part has a second bank that the flash algorithms cannot reach, and is erased through the
/// SROM API instead (see [`super::psoc_c3_erase`]).
///
/// The read is best-effort — any transport error is returned to the caller, which may choose
/// to ignore it. Attach must not fail just because this register could not be read; treating
/// the device as single-bank keeps the stock behaviour.
pub(super) fn detect_flash_bank_mode(
    iface: &mut dyn ArmDebugInterface,
    ap: &FullyQualifiedApAddress,
) -> Result<FlashBankMode, ArmError> {
    let ctl = {
        let mut mem = iface.memory_interface(ap)?;
        mem.read_word_32(FLASHC_FLASH_CTL)?
    };

    let mode = if ctl & FLASHC_FLASH_CTL_BANK != 0 {
        FlashBankMode::Dual
    } else {
        FlashBankMode::Single
    };

    tracing::info!(
        "PSOC C3: FLASHC_FLASH_CTL={:#010x} → {:?} flash bank mode",
        ctl,
        mode
    );

    Ok(mode)
}

/// CM33 access port base address, identical across every PSOC C3 family.
///
/// The `__apid` index the CMSIS pack assigns to this AP differs between families, but the
/// address it resolves to does not.
pub(super) const CM33_AP_BASE: u64 = 0xF000_2000;

/// The CM33 AP for the given debug port.
pub(super) fn cm33_ap(dp: DpAddress) -> FullyQualifiedApAddress {
    FullyQualifiedApAddress::v2_with_dp(dp, ApV2Address::new(CM33_AP_BASE))
}

/// SRSS soft reset control register (secure alias) — triggers a system soft reset.
///
/// For the M3/M5/P2/P5 family this is already the secure alias. The
/// x7/x8 family defines the non-secure base (`0x4220_0410`) and conditionally ORs in
/// the secure alias offset, which yields this same address.
pub(super) const SRSS_RES_SOFT_CTL: u32 = 0x5220_0410;
/// `SRSS_RES_SOFT_CTL.TRIG_SOFT` — triggers an immediate system reset.
pub(super) const SRSS_RES_SOFT_CTL_TRIG_SOFT: u32 = 1;

/// Apply the standard TrustZone-aware CM33 AP `CSW` and log the flash bank mode.
///
/// Reads the AP `CSW`, warns if the AP is closed (`DeviceEn == 0`, which typically
/// means a debug-certificate WFA unlock is required — not supported here), then
/// rewrites `CSW` with word-width privileged access and `HNONSEC` derived from
/// `SDeviceEn`. Finally performs a best-effort flash-bank-mode detection.
///
/// Shared by the generic `debug_device_unlock` and reused after a soft reset to
/// re-apply the `CSW` (the SRSS soft reset clears it).
pub(super) fn apply_standard_cm33_csw(
    interface: &mut dyn ArmDebugInterface,
    ap: &FullyQualifiedApAddress,
) -> Result<(), ArmError> {
    let csw = interface.read_raw_ap_register(ap, AP_CSW)?;
    let csw_bits = Cm33ApCsw(csw);

    if !csw_bits.device_en() {
        // The vendor unlock flow from here uploads a signed debug certificate
        // from the host and requests a WFA unlock — not implemented, since
        // probe-rs has no certificate-file infrastructure.
        tracing::warn!(
            "PSOC C3: CM33 AP is closed (DeviceEn=0), CSW: 0x{:08X}. \
             This chip may require a debug-certificate WFA unlock, which is not \
             supported yet — subsequent AP register accesses will likely fail.",
            csw
        );
    }

    let new_csw = Cm33ApCsw::with_standard_access(csw);

    tracing::debug!(
        "PSOC C3: AP CSW 0x{:08X} -> 0x{:08X} (SDeviceEn={}, HNONSEC={})",
        csw,
        new_csw.0,
        csw_bits.s_device_en(),
        new_csw.hnonsec(),
    );

    interface.write_raw_ap_register(ap, AP_CSW, new_csw.0)?;

    // Detect and log the flash bank mode (single vs dual). Best-effort — a read
    // failure here must not abort attach, so the error is intentionally ignored.
    let _ = detect_flash_bank_mode(interface, ap);

    Ok(())
}

/// Clear the DP sticky-error flags latched by the soft reset.
///
/// The SRSS soft-reset write faults its own transaction — the AHB fabric drops the access as
/// the SoC resets — which latches `STICKYERR`/`WDATAERR`/… . While those are set every
/// subsequent AP access faults with "communication with an access port or debug port", so
/// they have to be cleared before the AP is touched again. Best-effort: if the DP write
/// itself cannot get through there is nothing better to do than report it.
pub(super) fn clear_dp_sticky_errors(interface: &mut dyn ArmDebugInterface, dp: DpAddress) {
    let mut abort = Abort(0);
    abort.set_orunerrclr(true);
    abort.set_wderrclr(true);
    abort.set_stkerrclr(true);
    abort.set_stkcmpclr(true);

    if let Err(e) = interface.write_raw_dp_register(dp, Abort::ADDRESS, abort.0) {
        tracing::warn!("PSOC C3: failed to clear the DP sticky errors after reset: {e:?}");
    }
}

/// Halt the CM33 and trigger an SRSS soft reset through the SYS AP, then wait for
/// the reset to complete.
///
/// The default `AIRCR.SYSRESETREQ` reset does not work on these devices; the vendor
/// CMSIS sequence uses `SRSS_RES_SOFT_CTL` via the SYS AP, which works regardless of
/// CM33 AP state. The core is halted first so that user code holding the AHB bus
/// cannot stall the SYS AP write and hang the AHB fabric until a power cycle.
///
/// On return the reset has completed but the CM33 AP `CSW` has been cleared by the
/// reset; callers must re-apply it (see [`apply_standard_cm33_csw`]).
pub(super) fn halt_and_soft_reset(
    interface: &mut dyn ArmMemoryInterface,
    delay: Duration,
) -> Result<(), ArmError> {
    // Halt the CM33 before writing SRSS. If user code holds the AHB bus the
    // SYS AP write stalls and permanently hangs the AHB fabric until power cycle.
    let mut dhcsr = Dhcsr(0);
    dhcsr.set_c_debugen(true);
    dhcsr.set_c_halt(true);
    dhcsr.enable_write();
    let _ = interface.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into());

    let dp = interface.fully_qualified_address().dp();
    let sys_ap = sys_ap(dp);

    {
        let arm = interface.get_arm_debug_interface()?;
        arm.write_raw_ap_register(&sys_ap, AP_CSW, SysApCsw::secure_word().0)?;
        // Write triggers an immediate soft reset; transaction error is expected.
        let _ = write_mem32(arm, &sys_ap, SRSS_RES_SOFT_CTL, SRSS_RES_SOFT_CTL_TRIG_SOFT);
        // Flush the batch so the DRW write reaches the chip before we sleep —
        // otherwise the write can be deferred until the first post-reset read,
        // which then races the reset itself.
        let _ = arm.flush();
    }

    tracing::debug!(
        "PSOC C3: reset_system — waiting {}ms for boot ROM",
        delay.as_millis()
    );
    thread::sleep(delay);

    cortex_m_wait_for_reset(interface)?;

    Ok(())
}
