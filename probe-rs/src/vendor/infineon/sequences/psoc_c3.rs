//! Debug sequences for PSOC C3 (M3/M5/P2/P5) devices.
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

use std::{sync::Arc, thread, time::Duration};

use bitfield::bitfield;

use crate::{
    MemoryMappedRegister, Permissions,
    architecture::arm::{
        ApV2Address, ArmDebugInterface, ArmError, FullyQualifiedApAddress,
        core::armv7m::Dhcsr,
        dp::DpAddress,
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, cortex_m_wait_for_reset},
    },
};

const AP_CSW: u64 = 0xD00;
const AP_TAR: u64 = 0xD04;
const AP_DRW: u64 = 0xD0C;

/// SYS AP base address (`__apid=0`) — Infineon's proprietary bus-access AP, used to
/// reach `SRSS_RES_SOFT_CTL` regardless of the CM33 AP's lock state.
const SYS_AP_BASE: u64 = 0xF000_0000;

/// SRSS soft reset control register — already the secure alias for this family,
/// unlike the x7/x8 family which defines the non-secure base address and
/// conditionally ORs in the secure alias offset.
const SRSS_RES_SOFT_CTL: u32 = 0x5220_0410;
/// `SRSS_RES_SOFT_CTL.TRIG_SOFT` — triggers an immediate system reset.
const SRSS_RES_SOFT_CTL_TRIG_SOFT: u32 = 1;

/// Post-reset settle time — the boot ROM needs this long to re-open the AP after a
/// soft reset. This family settles faster than x7/x8, which uses 400ms instead.
const RESET_DELAY_MS: u64 = 350;

bitfield! {
    /// SYS AP CSW register — Infineon proprietary bus-access AP.
    ///
    /// Bit layout matches the standard AMBA AHB-AP CSW layout (ADI spec C2.2).
    #[derive(Clone, Copy)]
    struct SysApCsw(u32);
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
    fn secure_word() -> Self {
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
    struct Cm33ApCsw(u32);
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
    fn with_standard_access(csw: u32) -> Self {
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

/// Generic PSOC C3 debug sequence for devices without a custom debug-cert
/// unlock flow (M3/M5/P2/P5).
#[derive(Debug)]
pub struct PsocC3;

impl PsocC3 {
    /// Creates the debug sequence for a generic PSOC C3 chip.
    pub fn create() -> Arc<Self> {
        Arc::new(PsocC3)
    }

    fn sys_ap(dp: DpAddress) -> FullyQualifiedApAddress {
        FullyQualifiedApAddress::v2_with_dp(dp, ApV2Address(Some(SYS_AP_BASE)))
    }

    /// Write one 32-bit word to `addr` through the given AP using raw TAR/DRW register
    /// writes (the SYS AP is Infineon's proprietary bus-access portal, not a standard
    /// AHB/AXI AP, so it cannot go through the generic memory-interface adapter).
    fn write_mem32(
        iface: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
        addr: u32,
        val: u32,
    ) -> Result<(), ArmError> {
        iface.write_raw_ap_register(ap, AP_TAR, addr)?;
        iface.write_raw_ap_register(ap, AP_DRW, val)?;
        Ok(())
    }
}

impl ArmDebugSequence for PsocC3 {
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
        let csw = interface.read_raw_ap_register(default_ap, AP_CSW)?;
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

        interface.write_raw_ap_register(default_ap, AP_CSW, new_csw.0)?;

        Ok(())
    }

    /// Resets the SoC via `SRSS_RES_SOFT_CTL` (through the SYS AP) and re-applies the
    /// CM33 AP CSW correction afterwards.
    ///
    /// The default AIRCR.SYSRESETREQ-based reset doesn't work on this device — the
    /// vendor CMSIS sequence uses `SRSS_RES_SOFT_CTL` via the SYS AP instead, which
    /// works regardless of CM33 AP state. The SRSS soft reset also clears the CM33
    /// AP CSW (HNONSEC/SDeviceEn), so without re-applying it here, the very next AP
    /// register access (e.g. halting the core to run the flash algorithm) fails
    /// with "Target device did not respond to request".
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Halt the CM33 before writing SRSS. If user code holds the AHB bus the
        // SYS AP write stalls and permanently hangs the AHB fabric until power cycle.
        let mut dhcsr = Dhcsr(0);
        dhcsr.set_c_debugen(true);
        dhcsr.set_c_halt(true);
        dhcsr.enable_write();
        let _ = interface.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into());

        let dp = interface.fully_qualified_address().dp();
        let sys_ap = Self::sys_ap(dp);
        let cm33_ap = interface.fully_qualified_address();

        {
            let arm = interface.get_arm_debug_interface()?;
            arm.write_raw_ap_register(&sys_ap, AP_CSW, SysApCsw::secure_word().0)?;
            // Write triggers an immediate soft reset; transaction error is expected.
            let _ = Self::write_mem32(arm, &sys_ap, SRSS_RES_SOFT_CTL, SRSS_RES_SOFT_CTL_TRIG_SOFT);
            // Flush the batch so the DRW write reaches the chip before we sleep —
            // otherwise the write can be deferred until the first post-reset read,
            // which then races the reset itself.
            let _ = arm.flush();
        }

        tracing::debug!(
            "PSOC C3: reset_system — waiting {}ms for boot ROM",
            RESET_DELAY_MS
        );
        thread::sleep(Duration::from_millis(RESET_DELAY_MS));

        cortex_m_wait_for_reset(interface)?;

        // SRSS soft reset clears the CM33 AP CSW — re-apply HNONSEC/access fields
        // so the very next AP register access doesn't fail.
        let arm = interface.get_arm_debug_interface()?;
        self.debug_device_unlock(arm, &cm33_ap, &Permissions::new())
    }
}
