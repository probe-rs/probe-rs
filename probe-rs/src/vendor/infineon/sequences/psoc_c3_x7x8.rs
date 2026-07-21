//! Debug sequences for PSoC C3 x7/x8 (CAT1B P7/P8/M7/M8) devices.
//!
//! The DP (ADIv6 DPv3 MinDP, `debugconfig dormant="1"`) starts in dormant state and
//! only responds after the DORMANT-to-SWD selection alert sequence (ARM ADI §B4.3.4).
//! A failed DPIDR read returns the DP to dormant, so the full sequence must be re-sent
//! before every attempt — this is why we implement `try_dormant_connect` rather than
//! delegating to `DefaultArmSequence::debug_port_setup`, which exhausts up to 5 s of
//! per-attempt timeouts before the XRES fallback, exceeding the BootROM debug window.
//!
//! # PPCA core support (x7/x8 variants)
//!
//! x7 devices contain one extra Cortex-M33 PPCA core; x8 devices contain two. Each
//! PPCA core lives in its own power domain and must be initialised before the debug
//! access port for that core becomes functional.  `ppca_acquire` performs this
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
//! access.  The idempotency check in `ppca_acquire` makes repeated calls safe:
//! cold-start initialisation runs once; subsequent accesses are no-ops.

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bitfield::bitfield;
use probe_rs_target::Chip;

use crate::{
    MemoryMappedRegister, Permissions,
    architecture::arm::{
        ApV2Address, ArmDebugInterface, ArmError, FullyQualifiedApAddress, RegisterAddress,
        communication_interface::DapProbe,
        core::armv7m::Dhcsr,
        dp::{Ctrl, DPIDR, DpAccess, DpAddress, DpRegister},
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, DefaultArmSequence, cortex_m_wait_for_reset},
    },
    config::CoreExt,
};

/// RAM address where the debug certificate is loaded (mandatory for x7/x8 series).
const DEBUG_CERTIFICATE: u32 = 0x3400_4000;

/// SYS AP base address (`__apid=0`).
const SYS_AP_BASE: u64 = 0xF000_0000;
/// CM33 AP base address (`__apid=1`).
const CM33_AP_BASE: u64 = 0xF000_2000;
/// PPCA Core0 AP base address.
const PPCA0_AP_BASE: u64 = 0xF000_6000;
/// PPCA Core1 AP base address (x8 only).
const PPCA1_AP_BASE: u64 = 0xF000_8000;

// ADIv6 AP register offsets (same layout for all APv2 APs).
const AP_CSW: u64 = 0xD00;
const AP_TAR: u64 = 0xD04;
const AP_DRW: u64 = 0xD0C;

// ---------------------------------------------------------------------------
// PPCA hardware register addresses (CM33 secure address map, `0x5xxx_xxxx`).
// ---------------------------------------------------------------------------

/// PERI group 4 select — powers/clocks the PPCA domain (bit 0).
const PERI0_GR4_SL_CTL: u64 = 0x5200_4110;

/// PPCA CPU enable register — bit 0 = Core0 enable, bit 1 = Core1 enable.
const PPCA_CNFG_CPU_CTRL: u64 = 0x5300_0110;
/// PPCA CPU reset release register — bit 0 = Core0, bit 1 = Core1.
const PPCA_CNFG_RST_CTRL: u64 = 0x5300_0114;
/// PPCA Core0 CM33 control — write 0 to clear `CPU_WAIT` and let core start.
const PPCA_MXCM330_CM33_CTL: u64 = 0x5308_0000;
/// PPCA Core1 CM33 control — write 0 to clear `CPU_WAIT` (x8 only).
const PPCA_MXCM331_CM33_CTL: u64 = 0x5309_0000;
/// PPCA CPUSS access-port enable register.
const PPCA_CPUSS_AP_CTL: u64 = 0x530F_0000;
/// Main CPUSS AP control — must be updated so the PPCA AP is visible.
const CPUSS_AP_CTL: u64 = 0x521C_1000;

// PPCA SRAM base addresses from the CM33 (main-core) address map.
/// PPCA Core0 code SRAM base (`0x4301_0000`, 32 KiB).
const PPCA0_CODE_SRAM: u64 = 0x4301_0000;
/// PPCA Core1 code SRAM base (`0x4303_0000`, 32 KiB, x8 only).
const PPCA1_CODE_SRAM: u64 = 0x4303_0000;

/// Two ARM Thumb `B .` (endless-loop) instructions — used as the PPCA stub body.
const ENDLESS_LOOP_THUMB: u32 = 0xE7FE_E7FE;
/// Initial MSP value placed at vector-table[0] in the stub (top of the PPCA data SRAM).
const STUB_MSP: u32 = 0x2000_1FFC;
/// Reset_Handler address in the stub: byte offset 8 in the PPCA code SRAM (from the core's
/// own address space), Thumb bit set.
///
/// The code SRAM region (`0x0000_0000` from the PPCA core's view) is executable on the
/// default ARMv8-M memory map.  The data SRAM (`0x2000_0000`) is XN (Execute Never) —
/// directing the Reset_Handler there would immediately cause a HardFault.
const STUB_RESET_HANDLER: u32 = 0x0000_0009;

/// Value written to `PPCA_CPUSS_AP_CTL` to enable all PPCA access ports.
const PPCA_AP_CTL_ENABLE: u32 = 0x337;
/// Value written to `CPUSS_AP_CTL` to expose the PPCA AP to the main CPUSS.
const CPUSS_AP_CTL_ENABLE: u32 = 0x0000_53F7;

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
    /// CM33 AP CSW register.
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

/// Post-reset settle time: 400 ms, matching the BootROM debug window duration.
const RESET_DELAY_MS: u64 = 400;

/// PSoC C3 x7/x8 (CAT1B P7/P8/M7/M8) debug sequences.
#[derive(Debug)]
pub struct PsocC3X7X8 {
    /// The CM33 access port (primary core, used to initialise PPCA SRAM).
    cm33_ap: FullyQualifiedApAddress,
    /// PPCA access ports, in core-index order: index 0 = PPCA Core0, index 1 = PPCA Core1.
    ppca_aps: Vec<FullyQualifiedApAddress>,
}

impl PsocC3X7X8 {
    /// Creates debug sequences for a PSoC C3 x7/x8 chip.
    pub fn create(chip: &Chip) -> Arc<Self> {
        let dp = DpAddress::Default;

        // The CM33 AP is always at base 0xF000_2000.
        let cm33_ap = FullyQualifiedApAddress::v2_with_dp(dp, ApV2Address::new(CM33_AP_BASE));

        // Collect PPCA APs in deterministic order: Core0 (0xF0006000), Core1 (0xF0008000).
        let mut ppca_aps = Vec::new();
        let has_ppca0 = chip
            .cores
            .iter()
            .any(|c| matches!(c.memory_ap(), Some(ap) if ap == FullyQualifiedApAddress::v2_with_dp(dp, ApV2Address::new(PPCA0_AP_BASE))));
        let has_ppca1 = chip
            .cores
            .iter()
            .any(|c| matches!(c.memory_ap(), Some(ap) if ap == FullyQualifiedApAddress::v2_with_dp(dp, ApV2Address::new(PPCA1_AP_BASE))));

        if has_ppca0 {
            ppca_aps.push(FullyQualifiedApAddress::v2_with_dp(
                dp,
                ApV2Address::new(PPCA0_AP_BASE),
            ));
        }
        if has_ppca1 {
            ppca_aps.push(FullyQualifiedApAddress::v2_with_dp(
                dp,
                ApV2Address::new(PPCA1_AP_BASE),
            ));
        }

        Arc::new(PsocC3X7X8 { cm33_ap, ppca_aps })
    }

    fn sys_ap(dp: DpAddress) -> FullyQualifiedApAddress {
        FullyQualifiedApAddress::v2_with_dp(dp, ApV2Address(Some(SYS_AP_BASE)))
    }

    fn cm33_ap(dp: DpAddress) -> FullyQualifiedApAddress {
        FullyQualifiedApAddress::v2_with_dp(dp, ApV2Address(Some(CM33_AP_BASE)))
    }

    /// Write one 32-bit word to `addr` through the given AP using raw TAR/DRW register writes.
    ///
    /// `memory_interface()` cannot be used for the SYS AP: that function initialises a
    /// standard AMBA memory-AP adapter and resets CSW to a generic default, which clears
    /// the SYS AP's `DbgSwEnable` bit and makes subsequent transfers fail.  The SYS AP is
    /// Infineon's proprietary bus-access portal, not a standard AHB/AXI AP.
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

    /// Re-send the DORMANT-to-SWD sequence and try to read DPIDR.
    ///
    /// A failed DPIDR read returns this DP to dormant (ARM ADI §B4.3.4), so the
    /// complete sequence must be re-sent before every attempt. `DefaultArmSequence`
    /// cannot be reused here: its `debug_port_connect` waits up to 1 s per DPIDR
    /// attempt, so on a closed BootROM window it exhausts up to 5 s before the XRES
    /// fallback — longer than the window itself.
    ///
    /// Returns `Ok(true)` on success, `Ok(false)` on timeout.
    fn try_dormant_connect(
        &self,
        interface: &mut dyn DapProbe,
        timeout: Duration,
    ) -> Result<bool, ArmError> {
        let deadline = Instant::now() + timeout;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            // DORMANT-to-SWD sequence (ARM ADI §B4.3.4):
            // line reset → JTAG-to-Dormant → 8-cycle preamble → 128-bit alert → SWD activation →
            // line reset → idle
            let ok = interface.swj_sequence(51, 0x0007_FFFF_FFFF_FFFF).is_ok()
                && interface.swj_sequence(31, 0x33BB_BBBA).is_ok()
                && interface.swj_sequence(8, 0xFF).is_ok()
                && interface.swj_sequence(64, 0x8685_2D95_6209_F392).is_ok()
                && interface.swj_sequence(64, 0x19BC_0EA2_E3DD_AFE9).is_ok()
                && interface.swj_sequence(12, 0x1A0).is_ok()
                && interface.swj_sequence(51, 0x0007_FFFF_FFFF_FFFF).is_ok()
                && interface.swj_sequence(3, 0x00).is_ok();
            if ok {
                match interface.raw_read_register(RegisterAddress::DpRegister(DPIDR::ADDRESS)) {
                    Ok(v) => {
                        tracing::debug!(
                            "PSoC C3 x7/x8: attempt {attempt}: DPIDR=0x{v:08X} — connected"
                        );
                        return Ok(true);
                    }
                    Err(e) => {
                        tracing::debug!("PSoC C3 x7/x8: attempt {attempt}: DPIDR NACK: {e}");
                    }
                }
            }
            if Instant::now() >= deadline {
                tracing::debug!(
                    "PSoC C3 x7/x8: try_dormant_connect timed out after {attempt} attempts"
                );
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Returns `true` when `ap` is one of the PPCA access ports for this chip.
    fn is_ppca_ap(&self, ap: &FullyQualifiedApAddress) -> bool {
        self.ppca_aps.contains(ap)
    }

    /// Returns the 0-based PPCA core index (0 or 1) for `ap`, if it is a PPCA AP.
    fn ppca_core_index(&self, ap: &FullyQualifiedApAddress) -> Option<usize> {
        self.ppca_aps.iter().position(|a| a == ap)
    }

    /// Power up the PPCA domain, start the requested core in an endless-loop stub,
    /// and open the PPCA debug access port so the debugger can connect to it.
    ///
    /// Corresponds to the `ppca_acquire` + `attach_ppca_core` procedures in the
    /// reference OpenOCD TCL scripts.
    ///
    /// `core_index` selects which PPCA core to initialise: 0 = Core0, 1 = Core1.
    /// Both cores share a single call to step 1 (power-on) and step 5 (open AP),
    /// so calling this for each PPCA core in sequence is safe.
    ///
    /// When `force_full_acquire` is `true` the idempotency check is bypassed and
    /// the full reset/stub/AP-open cycle always runs.  Pass `true` from
    /// `reset_system` (recovering from an SRSS soft reset) and `false` from
    /// `on_attach` (re-attach without reset, where preserving debug state matters).
    fn ppca_acquire(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_index: usize,
        force_full_acquire: bool,
    ) -> Result<(), ArmError> {
        tracing::debug!(
            "PSoC C3 x7/x8: ppca_acquire for PPCA Core{core_index} \
             (force_full_acquire={force_full_acquire})"
        );

        // All register accesses go through the main CM33 AP memory interface.
        // The CM33 AP is open and in secure debug mode after debug_device_unlock.
        let mut cm33 = interface.memory_interface(&self.cm33_ap)?;

        // Step 1. Power the PPCA peripheral group (PERI0_GR4_SL_CTL, bit 0).
        // Required before any access to PPCA SRAM or subsystem registers.
        let gr4_val = cm33.read_word_32(PERI0_GR4_SL_CTL)?;
        cm33.write_word_32(PERI0_GR4_SL_CTL, gr4_val | 0x1)?;
        tracing::debug!(
            "PSoC C3 x7/x8: PPCA domain powered (PERI0_GR4_SL_CTL = 0x{:08X})",
            gr4_val | 0x1
        );

        // Idempotency check: if the AP is already open and the stub content matches,
        // the core was initialised in a previous attach — skip the reset/stub cycle.
        // Checking stub content (not just register flags) catches a stale session
        // where the core is stuck in HardFault with a corrupt or missing stub.
        let cpu_bit: u32 = 1 << core_index;
        let cpuss_ap_ctl_cur = cm33.read_word_32(CPUSS_AP_CTL).unwrap_or(0);
        let rst_ctrl_cur = cm33.read_word_32(PPCA_CNFG_RST_CTRL).unwrap_or(0);
        let cpu_ctrl_cur = cm33.read_word_32(PPCA_CNFG_CPU_CTRL).unwrap_or(0);

        let code_sram = match core_index {
            0 => PPCA0_CODE_SRAM,
            1 => PPCA1_CODE_SRAM,
            _ => {
                return Err(ArmError::Other(format!(
                    "PSoC C3 x7/x8: invalid PPCA core index {core_index}"
                )));
            }
        };

        let stub_w0 = cm33.read_word_32(code_sram).unwrap_or(0);
        let stub_w1 = cm33.read_word_32(code_sram + 4).unwrap_or(0);
        let stub_w2 = cm33.read_word_32(code_sram + 8).unwrap_or(0);

        let stub_valid =
            stub_w0 == STUB_MSP && stub_w1 == STUB_RESET_HANDLER && stub_w2 == ENDLESS_LOOP_THUMB;
        // Both AP enable registers must be set. A BSP init or killed session can clear
        // PPCA_CPUSS_AP_CTL while CPUSS_AP_CTL remains set — checking only the latter
        // gives a false-positive idempotency hit that leaves the AP broken.
        let ppca_ap_ctl_cur = cm33.read_word_32(PPCA_CPUSS_AP_CTL).unwrap_or(0);
        let ap_open = (cpuss_ap_ctl_cur & CPUSS_AP_CTL_ENABLE) == CPUSS_AP_CTL_ENABLE
            && (ppca_ap_ctl_cur & PPCA_AP_CTL_ENABLE) == PPCA_AP_CTL_ENABLE;
        let core_enabled = (rst_ctrl_cur & cpu_bit) != 0 && (cpu_ctrl_cur & cpu_bit) != 0;

        // All three conditions must hold for the idempotency skip to apply.
        // force_full_acquire=true (from reset_system) bypasses the check: after
        // an SRSS soft reset the core's debug domain is not properly initialised
        // even if the AP appears open, and a fresh reset/stub/halt cycle is needed.
        let skip_reset_stub_cycle = !force_full_acquire && stub_valid && ap_open && core_enabled;

        if skip_reset_stub_cycle {
            tracing::debug!(
                "PSoC C3 x7/x8: PPCA Core{core_index} already acquired — skipping reinitialisation"
            );
        } else {
            tracing::debug!(
                "PSoC C3 x7/x8: PPCA Core{core_index} cold start \
                 (stub_valid={stub_valid}, ap_open={ap_open}, core_enabled={core_enabled})"
            );
        }

        if !skip_reset_stub_cycle {
            // Step 2. Assert reset to clear any leftover HardFault / exception state
            // before writing the new stub.  bit SET = running, bit CLEAR = held in reset.
            cm33.write_word_32(PPCA_CNFG_CPU_CTRL, cpu_ctrl_cur | cpu_bit)?;
            cm33.write_word_32(PPCA_CNFG_RST_CTRL, rst_ctrl_cur & !cpu_bit)?; // assert reset
            tracing::debug!("PSoC C3 x7/x8: PPCA Core{core_index} reset asserted");

            // Step 3. Write a minimal stub (MSP + Reset_Handler + `B .` loop) into
            // code SRAM.  Data SRAM (0x2000_0000) is XN on ARMv8-M default map,
            // so the stub must live in code SRAM (0x0000_0000 from the core's view).
            cm33.write_word_32(code_sram, STUB_MSP)?;
            cm33.write_word_32(code_sram + 4, STUB_RESET_HANDLER)?;
            cm33.write_word_32(code_sram + 8, ENDLESS_LOOP_THUMB)?;
            tracing::debug!(
                "PSoC C3 x7/x8: PPCA Core{core_index} stub written to \
                 code_sram=0x{:08X}",
                code_sram
            );

            // Step 4. Deassert reset and clear CPU_WAIT so the core starts
            // executing from the vector table (picks up the stub from step 3).
            cm33.write_word_32(PPCA_CNFG_RST_CTRL, rst_ctrl_cur | cpu_bit)?; // deassert

            let cm33_ctl_addr = match core_index {
                0 => PPCA_MXCM330_CM33_CTL,
                1 => PPCA_MXCM331_CM33_CTL,
                _ => unreachable!(),
            };
            cm33.write_word_32(cm33_ctl_addr, 0x0)?; // clear CPU_WAIT → core starts
            tracing::debug!("PSoC C3 x7/x8: PPCA Core{core_index} released from reset");

            // Step 5. Open the PPCA AP: enable it inside the PPCA subsystem
            // (PPCA_CPUSS_AP_CTL) and make it visible from the main CPUSS (CPUSS_AP_CTL).
            cm33.write_word_32(PPCA_CPUSS_AP_CTL, PPCA_AP_CTL_ENABLE)?;
            cm33.write_word_32(CPUSS_AP_CTL, CPUSS_AP_CTL_ENABLE)?;
            tracing::debug!(
                "PSoC C3 x7/x8: PPCA Access Port opened \
                 (PPCA_CPUSS_AP_CTL=0x{PPCA_AP_CTL_ENABLE:08X}, \
                  CPUSS_AP_CTL=0x{CPUSS_AP_CTL_ENABLE:08X})"
            );

            // Step 6. Halt via DHCSR through the PPCA AP.
            // on_attach() fires on every session.core() access, so only halt on
            // cold start; the idempotency path preserves the core's running state.
            drop(cm33); // release CM33 AP borrow before opening the PPCA AP
            let ppca_ap = &self.ppca_aps[core_index];
            let mut ppca_mem = interface.memory_interface(ppca_ap)?;
            let mut dhcsr = Dhcsr(0);
            dhcsr.set_c_debugen(true);
            dhcsr.set_c_halt(true);
            dhcsr.enable_write();
            ppca_mem.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
            ppca_mem.flush()?;
            let deadline = Instant::now() + Duration::from_millis(100);
            loop {
                let dhcsr_val = Dhcsr(ppca_mem.read_word_32(Dhcsr::get_mmio_address())?);
                if dhcsr_val.s_halt() {
                    tracing::debug!(
                        "PSoC C3 x7/x8: PPCA Core{core_index} confirmed halted (S_HALT=1)"
                    );
                    break;
                }
                if Instant::now() >= deadline {
                    tracing::warn!(
                        "PSoC C3 x7/x8: PPCA Core{core_index} did not confirm halt within 100 ms"
                    );
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
            tracing::debug!("PSoC C3 x7/x8: PPCA Core{core_index} halt sequence complete");
        } else {
            tracing::debug!(
                "PSoC C3 x7/x8: PPCA Core{core_index} already acquired — core state preserved"
            );
        }

        Ok(())
    }
}

impl ArmDebugSequence for PsocC3X7X8 {
    /// Called before attaching to a core's AP.
    ///
    /// For PPCA cores this runs `ppca_acquire` to power the domain, load the
    /// stub, start the core, and open the debug AP before any register access.
    /// Unlike PSOC Edge's CM55 (gated by firmware via `CM55_WAIT`), `ppca_acquire`
    /// fully owns bring-up every time — there is no "not ready yet" state, so
    /// failures are propagated as-is rather than mapped to `ArmError::CoreDisabled`.
    fn on_attach(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        core_type: probe_rs_target::CoreType,
    ) -> Result<(), ArmError> {
        if let Some(core_index) = self.ppca_core_index(core_ap) {
            self.ppca_acquire(interface, core_index, false)
                .inspect_err(|e| {
                    tracing::warn!("PSoC C3 x7/x8: ppca_acquire for Core{core_index} failed: {e}");
                })
        } else {
            DefaultArmSequence(()).on_attach(interface, core_ap, core_type)
        }
    }

    /// `--connect-under-reset` is not supported on PSoC C3 x7/x8 devices.
    ///
    /// The debug port starts in dormant state and requires the chip to be powered
    /// and running before the DORMANT-to-SWD alert sequence can be sent.  Asserting
    /// nRESET before connecting would prevent `debug_port_setup` from waking the DP,
    /// so this override makes `--connect-under-reset` a no-op and emits a warning.
    fn reset_hardware_assert(&self, _interface: &mut dyn DapProbe) -> Result<(), ArmError> {
        tracing::warn!(
            "PSoC C3 x7/x8: `--connect-under-reset` is not supported — \
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
    /// its debug AP opened inside `on_attach`/`ppca_acquire`.
    fn debug_core_start(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        core_type: probe_rs_target::CoreType,
        debug_base: Option<u64>,
        cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        if self.is_ppca_ap(core_ap) {
            // PPCA AP initialisation is done in on_attach(); nothing more needed here.
            return Ok(());
        }
        DefaultArmSequence(()).debug_core_start(interface, core_ap, core_type, debug_base, cti_base)
    }

    fn debug_port_setup(
        &self,
        interface: &mut dyn DapProbe,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        // Phase 1: try to connect without resetting (400 ms = `__Reset_Finish_Delay`).
        // Handles fresh power-on, post-soft-reset reconnect, and stale SWD sessions.
        tracing::debug!("PSoC C3 x7/x8: Phase 1 — non-destructive dormant connect (400 ms)");
        if self.try_dormant_connect(interface, Duration::from_millis(400))? {
            tracing::debug!("PSoC C3 x7/x8: Phase 1 connected");
            return self.debug_port_connect(interface, dp);
        }

        // Phase 2: BootROM window has closed (user code running). Pulse XRES to
        // restart BootROM and re-open the window, then retry for 1.2 s.
        tracing::debug!("PSoC C3 x7/x8: Phase 1 failed — pulsing XRES to reopen BootROM window");
        match interface.target_reset() {
            Ok(()) => tracing::debug!("PSoC C3 x7/x8: Phase 2 — XRES pulsed"),
            Err(e) => tracing::debug!("PSoC C3 x7/x8: Phase 2 — XRES unavailable: {e}"),
        }
        thread::sleep(Duration::from_millis(10));

        tracing::debug!("PSoC C3 x7/x8: Phase 2 — dormant connect loop (1.2 s)");
        if self.try_dormant_connect(interface, Duration::from_millis(1200))? {
            tracing::debug!("PSoC C3 x7/x8: Phase 2 connected");
            return self.debug_port_connect(interface, dp);
        }

        tracing::warn!("PSoC C3 x7/x8: all connection attempts failed");
        self.debug_port_connect(interface, dp)
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
        tracing::debug!("PSoC C3 x7/x8: CM33 AP CSW = 0x{:08X}", csw);

        let csw_bits = Cm33ApCsw(csw);

        if !csw_bits.device_en() {
            tracing::warn!(
                "PSoC C3 x7/x8: DeviceEn=0 (AP closed) — sending WFA request and resetting"
            );

            let sys_ap = Self::sys_ap(dp);
            interface.write_raw_ap_register(&sys_ap, AP_CSW, SysApCsw::secure_word().0)?;

            // CTRL/STAT.READOK=1 means the previous AP read was in the secure domain.
            let ctrl_stat: Ctrl = interface.read_dp_register(dp)?;
            let sec_off: u32 = if ctrl_stat.read_ok() {
                SECURE_ALIAS_OFFSET
            } else {
                0
            };
            tracing::debug!("PSoC C3 x7/x8: domain_secure={}", ctrl_stat.read_ok());

            // Write cert address first — mandatory for x7/x8 series.
            Self::write_mem32(
                interface,
                &sys_ap,
                SrssBootDlmCtl2::ADDRESS | sec_off,
                DEBUG_CERTIFICATE,
            )?;

            // Set WFA request; boot ROM reads it after soft reset.
            Self::write_mem32(
                interface,
                &sys_ap,
                SrssBootDlmCtl::ADDRESS | sec_off,
                SrssBootDlmCtl::WFA_REQUEST_DEBUG_CERT,
            )?;

            // Trigger soft reset via SYS AP — SWD will drop, error is expected.
            let _ = Self::write_mem32(
                interface,
                &sys_ap,
                SrssResSoftCtl::ADDRESS | sec_off,
                SrssResSoftCtl::soft_reset_request().0,
            );
            // Flush the CMSIS-DAP batch so the DRW write reaches the chip before we sleep.
            let _ = interface.flush();

            tracing::debug!(
                "PSoC C3 x7/x8: waiting {}ms for boot ROM to process WFA request",
                RESET_DELAY_MS
            );
            thread::sleep(Duration::from_millis(RESET_DELAY_MS));

            // Signal probe-rs to re-do dormant wake-up + DP power-up.
            return Err(ArmError::ReAttachRequired);
        }

        // AP is open. Set HNONSEC based on SDeviceEn and restore all required
        // functional fields (Size=Word, AddrInc=Single, HPROT[0,1,3]).
        // probe-rs's amba_ahb3/5 also does this on AP init, but doing it explicitly
        // here ensures the hardware CSW is correct before the AP adapter reads it.
        let new_csw = Cm33ApCsw::with_standard_access(csw);

        tracing::debug!(
            "PSoC C3 x7/x8: CM33 AP CSW 0x{:08X} -> 0x{:08X} (SDeviceEn={}, HNONSEC={})",
            csw,
            new_csw.0,
            csw_bits.s_device_en(),
            new_csw.hnonsec(),
        );
        interface.write_raw_ap_register(&cm33_ap, AP_CSW, new_csw.0)?;

        Ok(())
    }

    /// Resets the SoC via `SRSS_RES_SOFT_CTL` through the SYS AP.
    ///
    /// The vendor CMSIS sequence uses `AIRCR.SYSRESETREQ`, but `SRSS_RES_SOFT_CTL`
    /// via the SYS AP works regardless of CM33 AP state.
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
        if self.is_ppca_ap(&interface.fully_qualified_address()) {
            tracing::debug!(
                "PSoC C3 x7/x8: reset_system — skipping for PPCA AP {:?} (reset handled by CM33 reset_system)",
                interface.fully_qualified_address()
            );
            return Ok(());
        }

        // Halt the CM33 before writing SRSS. If user code holds the AHB bus the
        // SYS AP write stalls and permanently hangs the AHB fabric until power cycle.
        let mut dhcsr = Dhcsr(0);
        dhcsr.set_c_debugen(true);
        dhcsr.set_c_halt(true);
        dhcsr.enable_write();
        let _ = interface.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into());

        tracing::debug!("PSoC C3 x7/x8: reset_system — SRSS soft reset via SYS AP");

        let dp = interface.fully_qualified_address().dp();
        let sys_ap = Self::sys_ap(dp);
        {
            let arm = interface.get_arm_debug_interface()?;
            arm.write_raw_ap_register(&sys_ap, AP_CSW, SysApCsw::secure_word().0)?;
            // Write triggers an immediate soft reset; transaction error is expected.
            let _ = Self::write_mem32(
                arm,
                &sys_ap,
                SrssResSoftCtl::ADDRESS | SECURE_ALIAS_OFFSET,
                SrssResSoftCtl::soft_reset_request().0,
            );
            // Flush the CMSIS-DAP batch so the DRW write reaches the chip before we sleep.
            // Without this, CMSIS-DAP batching defers the write until the first READ inside
            // cortex_m_wait_for_reset, causing the reset to fire mid-batch and the DHCSR
            // read to fail with FAULT (not NoAcknowledge), so reinitialize() is never called.
            let _ = arm.flush();
        }

        tracing::debug!(
            "PSoC C3 x7/x8: reset_system — waiting {}ms for boot ROM",
            RESET_DELAY_MS
        );
        thread::sleep(Duration::from_millis(RESET_DELAY_MS));

        // Wait for reset to complete. On NoAcknowledge reinitialize() calls
        // debug_port_setup; Phase 1 reconnects without XRES — warm reset
        // preserves SRSS_BOOT_DLM_CTL so BootROM re-opens the AP.
        cortex_m_wait_for_reset(interface)?;

        // SRSS soft reset may clear CM33 AP CSW. Re-apply all required fields
        // (Size=Word, AddrInc=Single, HPROT[0,1,3], HNONSEC) so that the AP adapter
        // can complete word-width accesses to PPB registers (e.g. DHCSR) without
        // faulting — the adapter's cached Size may still say U32, so it will not
        // automatically rewrite the hardware CSW.
        let cm33_ap = Self::cm33_ap(dp);
        {
            let arm = interface.get_arm_debug_interface()?;
            let csw = arm.read_raw_ap_register(&cm33_ap, AP_CSW)?;
            let new_csw = Cm33ApCsw::with_standard_access(csw);
            tracing::debug!(
                "PSoC C3 x7/x8: reset_system — CM33 AP CSW 0x{:08X} -> 0x{:08X} \
                 (SDeviceEn={}, HNONSEC={})",
                csw,
                new_csw.0,
                Cm33ApCsw(csw).s_device_en(),
                new_csw.hnonsec(),
            );
            arm.write_raw_ap_register(&cm33_ap, AP_CSW, new_csw.0)?;
        }

        // Re-acquire every PPCA core: re-power the domain, re-write the stub,
        // re-open the AP (PPCA_CPUSS_AP_CTL and CPUSS_AP_CTL are cleared by SRSS
        // soft reset), and halt via DHCSR.  CPU_CTRL / RST_CTRL are left at their
        // reset defaults (0x3) so the CM33 BSP skips PPCA IPC initialisation.
        {
            let arm = interface.get_arm_debug_interface()?;
            for (core_index, _) in self.ppca_aps.iter().enumerate() {
                tracing::debug!("PSoC C3 x7/x8: reset_system — re-acquiring PPCA Core{core_index}");
                if let Err(e) = self.ppca_acquire(arm, core_index, true) {
                    tracing::warn!(
                        "PSoC C3 x7/x8: reset_system — ppca_acquire for Core{core_index} failed: {e}"
                    );
                }
            }
        }

        Ok(())
    }
}
