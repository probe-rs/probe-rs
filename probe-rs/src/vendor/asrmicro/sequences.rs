//! Support for the ASR6601 device family.
//!
//! The ASR6601 gates the debug connection in low-power modes unless explicitly
//! allowed via SYSCFG (`SYSCFG_CR2` bit 10 for Deepsleep, `SYSCFG_CR3` bits
//! 1-0 for Stop/Standby; ASR6601 Reference Manual v1.5.0 §7.5.3/§7.5.4).
//! Without these bits, entering a low-power mode drops the SWD connection, so
//! this sequence enables them on connect (mirroring what the STM32 sequences
//! do with `DBGMCU_CR`). They are deliberately left set at session end: the
//! alternative (restore-on-exit) re-locks us out as soon as firmware sleeps.
//!
//! Additionally, resident firmware can permanently (until reset) disable the
//! SWD pins via the GPIO multiplexing register (one-way seal, RM §11.12), and
//! a sleeping application answers DP requests but never completes AP
//! transfers. To recover such targets, `debug_device_unlock` polls for an
//! awake window (duty-cycled firmware surfaces brief windows on its own).

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use probe_rs_target::CoreType;

use crate::{
    MemoryMappedRegister, RegisterId,
    architecture::arm::{
        ArmDebugInterface, ArmError,
        armv7m::Dhcsr,
        core::cortex_m::write_core_reg,
        dp::{Abort, DpAccess, DpAddress},
        memory::ArmMemoryInterface,
        sequences::ArmDebugSequence,
        traits::FullyQualifiedApAddress,
    },
};

#[derive(Debug)]
pub struct Asr6601;

impl Asr6601 {
    pub fn create() -> Arc<Self> {
        Arc::new(Self)
    }

    /// Establish debug-friendly system state and return the current CR3 value.
    ///
    /// Used on first contact (see `debug_device_unlock`); the reset emulation
    /// below reuses [`enable_debug_during_sleep`] directly.
    fn establish_debug(
        interface: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
    ) -> Result<u32, ArmError> {
        let mut memory = interface.memory_interface(ap)?;
        enable_debug_during_sleep(&mut *memory)?;
        memory.read_word_32(SYSCFG_CR3)
    }

    /// Log the last reset sources (best-effort, strictly read-only).
    ///
    /// `RST_SR` flags are sticky and cleared only by writing 1, so this must
    /// never write: reading identifies whether the last reboot came from a
    /// watchdog, brown-out, standby exit, CPU request, and more. Note the
    /// brown-out and standby flags live in the always-on domain and may
    /// predate the current boot.
    fn log_reset_sources(interface: &mut dyn ArmDebugInterface, ap: &FullyQualifiedApAddress) {
        const SOURCES: [(u32, &str); 7] = [
            (0, "standby"),
            (1, "security"),
            (2, "cpu"),
            (3, "flash-controller"),
            (4, "window-watchdog"),
            (5, "independent-watchdog"),
            (6, "brown-out"),
        ];
        if let Ok(mut memory) = interface.memory_interface(ap)
            && let Ok(sr) = memory.read_word_32(RCC_RST_SR)
        {
            let mut listed = String::new();
            for (bit, name) in SOURCES {
                if sr & (1 << bit) != 0 {
                    if !listed.is_empty() {
                        listed.push_str(", ");
                    }
                    listed.push_str(name);
                }
            }
            if listed.is_empty() {
                listed.push_str("none recorded");
            }
            tracing::debug!("ASR6601: reset sources since last clear: {listed}");
        }
    }

    /// Clear sticky error flags in the debug port (best-effort).
    ///
    /// A failed AP transfer can leave STICKYERR (and friends) set, which makes
    /// subsequent transfers fault until cleared. The debug port itself stays
    /// responsive throughout, so this cannot fail the recovery.
    fn clear_sticky_errors(interface: &mut dyn ArmDebugInterface, dp: DpAddress) {
        let mut abort = Abort(0);
        abort.set_dapabort(true);
        abort.set_orunerrclr(true);
        abort.set_wderrclr(true);
        abort.set_stkerrclr(true);
        abort.set_stkcmpclr(true);
        let _ = interface.write_dp_register(dp, abort);
    }

    /// Halt the core via DHCSR (write-every-poll, short timeout).
    ///
    /// A running application can re-enter low-power modes (or reconfigure
    /// clocks) at any moment, closing the awake window before flashing even
    /// starts. Halting immediately — while the AP is proven alive — converts
    /// a transient window into a stable session for the rest of the attach.
    fn halt_core(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        let start = Instant::now();
        loop {
            let mut dhcsr = Dhcsr(0);
            dhcsr.set_c_halt(true);
            dhcsr.set_c_debugen(true);
            dhcsr.enable_write();
            memory.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

            if Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?).s_halt() {
                return Ok(());
            }
            if start.elapsed() >= Duration::from_millis(500) {
                return Err(ArmError::Timeout);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

const VECTOR_TABLE: u64 = 0x0800_0000;
const FLASH_RANGE: core::ops::RangeInclusive<u32> = 0x0800_0000..=0x0803_FFFF;
// RAM is 0x20000000..0x20010000; the initial SP is the region end (0x20010000).
// The SE range covers CB (128K flash / 16K RAM) as a subset, so one range
// validates both.
const RAM_RANGE: core::ops::RangeInclusive<u32> = 0x2000_0000..=0x2001_0000;

const ICTR: u64 = 0xE000_E004;
const SYST_CSR: u64 = 0xE000_E010;
const SYST_RVR: u64 = 0xE000_E014;
const SYST_CVR: u64 = 0xE000_E018;
const NVIC_ICER: u64 = 0xE000_E180;
const NVIC_ICPR: u64 = 0xE000_E280;
const SCB_ICSR: u64 = 0xE000_ED04;
const SCB_VTOR: u64 = 0xE000_ED08;
const SCB_SCR: u64 = 0xE000_ED10;
const SCB_SHCSR: u64 = 0xE000_ED24;
const SCB_AIRCR: u64 = 0xE000_ED0C;
const AIRCR_VECTKEY: u32 = 0x5FA << 16;
const AIRCR_PRIGROUP_MASK: u32 = 0x700;
const FP_CTRL: u64 = 0xE000_2000;
const MPU_CTRL: u64 = 0xE000_ED94;

// RCC base 0x4000_0000 — RM §8.3.4 RCC_CGR0 (offset 0x00C), RST_SR (offset 0x020).
const RCC_CGR0: u64 = 0x4000_000C;
/// RM §8.3.4 bit 21: clock gate for the SYSCFG peripheral.
const RCC_CGR0_SYSCFG_CLK_EN: u32 = 1 << 21;
const RCC_RST_SR: u64 = 0x4000_0020;

// SYSCFG base 0x4000_1000 — RM §7.5.
const SYSCFG_CR2: u64 = 0x4000_1008;
const SYSCFG_CR3: u64 = 0x4000_100C;
/// SYSCFG_CR2 bit 10: allow debug while the CPU is in Sleep/Deepsleep.
const SYSCFG_DBG_SLEEP: u32 = 1 << 10;
/// SYSCFG_CR3 bit 1: allow debug while the CPU is in Stop.
const SYSCFG_DBG_STOP: u32 = 1 << 1;
/// SYSCFG_CR3 bit 0: allow debug while the CPU is in Standby.
const SYSCFG_DBG_STANDBY: u32 = 1 << 0;

/// Freeze counters while the core is halted (`SYSCFG_HALTED_*`, CR2).
///
/// Pauses timers and both watchdogs for the duration of a debug halt only;
/// running behavior is untouched. Without these, a watchdog started by
/// firmware could fire mid-session (e.g. during flashing). Bits without a
/// name here are not halt-related and are preserved by the RMW below.
const SYSCFG_HALTED_BASICTIM1: u32 = 1 << 19;
const SYSCFG_HALTED_BASICTIM0: u32 = 1 << 20;
const SYSCFG_HALTED_GPTIM3: u32 = 1 << 21;
const SYSCFG_HALTED_GPTIM2: u32 = 1 << 22;
const SYSCFG_HALTED_GPTIM1: u32 = 1 << 23;
const SYSCFG_HALTED_GPTIM0: u32 = 1 << 24;
const SYSCFG_HALTED_WWDG: u32 = 1 << 25;
const SYSCFG_HALTED_IWDG: u32 = 1 << 26;
const SYSCFG_HALTED_LPTIM0: u32 = 1 << 27;
const SYSCFG_HALTED_LPTIM1: u32 = 1 << 30;

const REG_SP: RegisterId = RegisterId(13);
const REG_LR: RegisterId = RegisterId(14);
const REG_PC: RegisterId = RegisterId(15);
const REG_XPSR: RegisterId = RegisterId(16);
const REG_MSP: RegisterId = RegisterId(17);
const REG_PSP: RegisterId = RegisterId(18);
// { CONTROL[7:0], FAULTMASK[7:0], BASEPRI[7:0], PRIMASK[7:0] }
const REG_SPECIAL: RegisterId = RegisterId(20);

/// Keep SWD working after the application executes WFI/WFE.
///
/// By default the ASR6601 powers down the debug connection when entering low-power
/// modes (Sleep / Stop / Standby). Once that happens the probe times out and cannot
/// reattach until a power cycle or BOOT0 recovery.
///
/// The chip has three sticky "keep debug on" bits in SYSCFG. We set all three so any
/// low-power mode is debug-safe. Timers/watchdogs are additionally frozen while
/// halted so a watchdog started by firmware cannot fire mid-session.
fn enable_debug_during_sleep(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    // SYSCFG_CR2 is on the gated SYSCFG clock; we need to enable it before touching CR2.
    // Gated off at reset: SYSCFG reads return zero and writes silently drop
    // until this is set.
    let cgr0 = memory.read_word_32(RCC_CGR0)?;
    if cgr0 & RCC_CGR0_SYSCFG_CLK_EN == 0 {
        memory.write_word_32(RCC_CGR0, cgr0 | RCC_CGR0_SYSCFG_CLK_EN)?;
    }

    // SYSCFG_DBG_SLEEP = 1 → "allowed" to keep a debug connection
    // in Sleep/Deepsleep (covers ordinary WFE/WFI with SLEEPDEEP = 0/1).
    let cr2 = memory.read_word_32(SYSCFG_CR2)?;
    memory.write_word_32(
        SYSCFG_CR2,
        cr2 | SYSCFG_DBG_SLEEP
            | SYSCFG_HALTED_BASICTIM1
            | SYSCFG_HALTED_BASICTIM0
            | SYSCFG_HALTED_GPTIM3
            | SYSCFG_HALTED_GPTIM2
            | SYSCFG_HALTED_GPTIM1
            | SYSCFG_HALTED_GPTIM0
            | SYSCFG_HALTED_WWDG
            | SYSCFG_HALTED_IWDG
            | SYSCFG_HALTED_LPTIM0
            | SYSCFG_HALTED_LPTIM1,
    )?;

    // SYSCFG_DBG_STOP / SYSCFG_DBG_STANDBY = 1 → keep debug
    // when firmware later enters Stop0–3 or Standby (SLEEPDEEP + PWR lp_mode).
    let cr3 = memory.read_word_32(SYSCFG_CR3)?;
    memory.write_word_32(SYSCFG_CR3, cr3 | SYSCFG_DBG_STOP | SYSCFG_DBG_STANDBY)?;

    Ok(())
}

impl ArmDebugSequence for Asr6601 {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        // Fast path: healthy targets establish debug state straight away (zero
        // behaviour change, `attach` semantics untouched).
        tracing::debug!("ASR6601: debug_device_unlock enter");
        let _cr3 = match Self::establish_debug(interface, default_ap) {
            Ok(cr3) => cr3,
            Err(first_err) => {
                // The AP is not completing transfers. On this chip that is
                // typical when resident firmware sleeps (debug power domain
                // gated) or one-way-disables the SWD pins (RM §11.12).
                // Poll for an awake window since duty-cycled firmware
                // surfaces brief windows on its own. Healthy targets never
                // take this branch, so their behaviour (and `attach`
                // semantics) is unchanged.
                tracing::debug!(
                    "ASR6601: initial debug setup failed ({first_err:?}), \
                     polling for an awake window"
                );
                // Poll densely: the chip may only surface brief awake windows
                // between sleep phases (duty-cycled firmware), so retry fast
                // for a few seconds rather than a few slow rounds.
                let mut attempt = Self::establish_debug(interface, default_ap);
                for _ in 0..39 {
                    if attempt.is_ok() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                    Self::clear_sticky_errors(interface, default_ap.dp());
                    attempt = Self::establish_debug(interface, default_ap);
                }
                attempt?
            }
        };

        // Forensics, best-effort: note what last rebooted the chip while the
        // AP is proven alive (see `log_reset_sources`).
        Self::log_reset_sources(interface, default_ap);

        // Freeze the core while the AP is proven alive (see `halt_core`).
        // A failed halt aborts the attach: without a halted core the window
        // would close mid-flash anyway.
        let mut memory = interface.memory_interface(default_ap)?;
        Self::halt_core(&mut *memory)?;

        Ok(())
    }

    /// SYSRESETREQ drops the ASR6601 debug connection and does not reliably boot
    /// the application under reset catch. Emulate the architectural reset state
    /// while preserving the debug connection.
    ///
    /// A real system reset (SYSRESETREQ) on the ASR6601 wipes the debug-domain
    /// state this sequence depends on (reset catch, DHCSR halt, SYSCFG debug
    /// bits): after any reset the application boots straight back into
    /// instant deep sleep with the AP down, and no subsequent halt ever lands.
    /// So instead of resetting, reconstruct the architectural reset state by
    /// hand on the already-halted core: freeze it, sanitize interrupt and
    /// system configuration, reload stack and entry point from the vector
    /// table, and re-apply the debug keep-alive bits. The flashing flow (which
    /// unconditionally resets before staging its algorithm) and the `reset`
    /// command both work through this path.
    ///
    /// Deliberately NOT touched, even though a real reset would clear them:
    /// `DEMCR` (the caller's reset catch may live there), and — as everywhere
    /// in this module — the SYSCFG debug bits are left set rather than
    /// restored, so follow-up sessions connect first try.
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::warn!(
            "ASR6601: emulating system reset without rebooting; a real reset \
             drops the debug connection on this chip"
        );

        // Freeze the core first; everything below needs a stable target.
        Self::halt_core(interface)?;

        let sp = interface.read_word_32(VECTOR_TABLE)?;
        let pc = interface.read_word_32(VECTOR_TABLE + 4)?;
        let vectors_valid =
            RAM_RANGE.contains(&sp) && FLASH_RANGE.contains(&(pc & !1)) && pc & 1 == 1;

        // Quiesce interrupt and system configuration (reset-equivalent values).
        interface.write_word_32(SYST_CSR, 0)?;
        interface.write_word_32(SYST_RVR, 0)?;
        interface.write_word_32(SYST_CVR, 0)?;
        let interrupt_registers = (interface.read_word_32(ICTR)? & 0x0f) + 1;
        for index in 0..interrupt_registers {
            let offset = u64::from(index) * 4;
            interface.write_word_32(NVIC_ICER + offset, u32::MAX)?;
            interface.write_word_32(NVIC_ICPR + offset, u32::MAX)?;
        }
        interface.write_word_32(SCB_ICSR, (1 << 25) | (1 << 27))?;
        interface.write_word_32(SCB_SCR, 0)?;
        interface.write_word_32(SCB_SHCSR, 0)?;
        // Restore default interrupt priority grouping: stale grouping would
        // mislead resumed firmware that never sets its own. AIRCR writes
        // require the key and must not set any reset bits.
        let aircr = interface.read_word_32(SCB_AIRCR)?;
        interface.write_word_32(SCB_AIRCR, AIRCR_VECTKEY | (aircr & !AIRCR_PRIGROUP_MASK))?;
        // Clear stale breakpoints and MPU regions: a real reset disables both,
        // and leftovers would halt or fault the freshly started firmware.
        // FP_CTRL KEY field must read 0b10 on write; ENABLE=0 disables.
        interface.write_word_32(FP_CTRL, 0x2)?;
        interface.write_word_32(MPU_CTRL, 0)?;
        // Data watchpoints and trace are deliberately left alone (unlike the
        // above): firmware timestamping may depend on the cycle counter, which
        // a real reset would only clear together with a reboot that
        // re-initializes it. Clearing it mid-session would freeze timestamps
        // with no recovery until the next reboot.

        if vectors_valid {
            interface.write_word_32(SCB_VTOR, VECTOR_TABLE as u32)?;
            write_core_reg(interface, REG_SPECIAL, 0)?;
            write_core_reg(interface, REG_XPSR, 1 << 24)?;
            write_core_reg(interface, REG_PSP, 0)?;
            write_core_reg(interface, REG_MSP, sp)?;
            write_core_reg(interface, REG_SP, sp)?;
            write_core_reg(interface, REG_LR, u32::MAX)?;
            write_core_reg(interface, REG_PC, pc)?;
        } else {
            // Erased or corrupt flash: do NOT fabricate register state from
            // garbage (an all-ones vector table would install 0xFFFFFFFF as
            // stack and entry point). Stay halted so flashing can proceed;
            // starting firmware requires valid vectors first.
            tracing::warn!(
                "vector table at {VECTOR_TABLE:#010x} is invalid (SP={sp:#010x}, PC={pc:#010x}), \
                 staying halted for flashing"
            );
        }

        // Re-apply the debug keep-alive bits (a real reset would clear them).
        enable_debug_during_sleep(interface)?;

        Ok(())
    }

    fn debug_core_stop(
        &self,
        _memory: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
    ) -> Result<(), ArmError> {
        // Deliberately do NOT restore SYSCFG_CR2/CR3 (unlike e.g. the STM32
        // sequences, which restore DBGMCU_CR here). Restoring would clear the
        // DBG_* bits at session end and re-lock us out as soon as firmware
        // (e.g. a WFE loop) sleeps — while the bits stay set, Sleep/Stop/
        // Standby cannot take the debug connection down, so follow-up
        // sessions connect first try. Power impact is irrelevant on a dev
        // board; firmware that cares about microamps should manage these bits
        // itself.
        Ok(())
    }
}
