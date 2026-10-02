//! Primitives shared by every Infineon Cortex-M debug sequence (PSOC C3 family and
//! PSOC Edge) that talks to a dormant SWJ-DP through Infineon's proprietary raw
//! AP bus-access convention.

use std::{
    error::Error,
    thread,
    time::{Duration, Instant},
};

use crate::MemoryMappedRegister;
use crate::architecture::arm::{
    ApV2Address, ArmDebugInterface, ArmError, DapAccess, DapError, FullyQualifiedApAddress, Pins,
    RegisterAddress,
    core::armv7m::{Aircr, Dhcsr},
    dp::{Abort, Ctrl, DPIDR, DpAddress, DpRegister},
    memory::ArmMemoryInterface,
    sequences::{ArmDebugSequence, DefaultArmSequence},
    traits::DebugPortWire,
};
use crate::probe::{BitSequence, WireProtocol, swd::Port};

/// AP CSW register offset (ADIv6 APv2 layout).
pub(super) const AP_CSW: u64 = 0xD00;
/// AP TAR register offset (ADIv6 APv2 layout).
pub(super) const AP_TAR: u64 = 0xD04;
/// AP DRW register offset (ADIv6 APv2 layout).
pub(super) const AP_DRW: u64 = 0xD0C;

/// SYS AP base address (`__apid=0`) — Infineon's proprietary bus-access AP, used to
/// reach chip-control registers regardless of the CM33/CM55 AP's lock state.
pub(super) const SYS_AP_BASE: u64 = 0xF000_0000;

pub(super) const CM33_AP_BASE: u64 = 0xF000_2000;

/// Boot-ROM post-reset settle and debug-window budget.
/// Shared by the Infineon reset-settle sleeps and the dormant-wake / reconnect retry
/// loops — how long the boot ROM takes to finish its handshake and (re-)open the
/// debug window after a reset.
pub(super) const RESET_FINISH_DELAY_MS: u64 = 400;

/// The SYS AP for the given debug port.
pub(super) fn sys_ap(dp: DpAddress) -> FullyQualifiedApAddress {
    FullyQualifiedApAddress::v2_with_dp(dp, ApV2Address(Some(SYS_AP_BASE)))
}

pub(super) fn cm33(dp: DpAddress) -> FullyQualifiedApAddress {
    FullyQualifiedApAddress::v2_with_dp(dp, ApV2Address(Some(CM33_AP_BASE)))
}

/// Power up the DP for the JTAG path by posting `CTRL/STAT` directly. No-op on SWD.
///
/// Writes `CTRL/STAT = 0x5000_0032` = `CSYSPWRUPREQ | CDBGPWRUPREQ` plus the
/// write-1-to-clear sticky flags (`STICKYERR | STICKYCMP | STICKYORUN` = `0x32`).
/// `MASKLANE` is intentionally omitted: both Infineon DPs are MinDPs (`DPIDR` MIN=1)
/// where those bits are unpredictable, so this matches probe-rs's default
/// `debug_port_start`. When `prime_abort` is set it posts an `ABORT` (all
/// error-clear bits) first.
///
/// Over JTAG the Infineon SWJ-DP NACKs DPACC reads (including the `DPIDR` read the
/// stock [`ArmDebugSequence::debug_port_start`](crate::architecture::arm::sequences::ArmDebugSequence::debug_port_start)
/// does first) until the DAP is powered up, so this doubles as the JTAG DAP power-up.
/// The `ABORT` prime flushes the stale ACK the raw-scan JTAG DP pipeline shifts out on
/// the first posted DPACC access. The `CTRL/STAT` write is posted, so it goes through even while
/// DPACC reads still NACK before power-up.
pub(super) fn jtag_dp_powerup(interface: &mut dyn DapAccess, dp: DpAddress, prime_abort: bool) {
    let is_jtag = interface.active_wire_protocol() == Some(WireProtocol::Jtag);
    if !is_jtag {
        return;
    }

    if prime_abort {
        let mut abort = Abort(0);
        abort.set_orunerrclr(true);
        abort.set_stkerrclr(true);
        abort.set_stkcmpclr(true);
        abort.set_wderrclr(true);
        let _ = interface.write_raw_dp_register(dp, Abort::ADDRESS, abort.into());
    }

    let _ = interface.write_raw_dp_register(dp, Ctrl::ADDRESS, 0x5000_0032);
}

/// Shared `debug_port_start` skeleton: run the stock power-up, and on failure post the
/// JTAG DAP power-up ([`jtag_dp_powerup`]) and retry once.
///
/// When `prime_before_first` is set the power-up is also posted *before* the first
/// attempt — needed where the SWJ-DP NACKs the stock `DPIDR` read until the DAP is
/// powered up. `prime_abort` is forwarded to [`jtag_dp_powerup`]. On SWD the power-up
/// posts are no-ops, so this reduces to the stock power-up with a single retry. Callers
/// that need a further fallback (e.g. an XRES reset-acquire) wrap the returned `Err`.
pub(super) fn jtag_dp_start_with_powerup(
    interface: &mut dyn DapAccess,
    dp: DpAddress,
    prime_abort: bool,
    prime_before_first: bool,
) -> Result<(), ArmError> {
    if prime_before_first {
        jtag_dp_powerup(interface, dp, prime_abort);
    }

    if DefaultArmSequence(())
        .debug_port_start(&mut *interface, dp)
        .is_ok()
    {
        return Ok(());
    }

    jtag_dp_powerup(interface, dp, prime_abort);
    DefaultArmSequence(()).debug_port_start(interface, dp)
}

/// Clear the SWD DP sticky-error flags via `ABORT`. No-op on JTAG.
///
/// Over SWD the CTRL/STAT sticky-error bits (`STICKYERR | STICKYCMP | STICKYORUN`) are
/// read-only, so sticky errors must be cleared by writing the matching clear bits to
/// `ABORT` (`ORUNERRCLR | WDERRCLR | STKERRCLR | STKCMPCLR` = `0x1E`). This is the SWD
/// counterpart of the JTAG write-1-to-clear of CTRL/STAT done in [`jtag_dp_powerup`].
pub(super) fn swd_dp_clear_sticky(interface: &mut dyn DapAccess, dp: DpAddress) {
    let is_jtag = interface.active_wire_protocol() == Some(WireProtocol::Jtag);
    if is_jtag {
        return;
    }

    let mut abort = Abort(0);
    abort.set_orunerrclr(true);
    abort.set_wderrclr(true);
    abort.set_stkerrclr(true);
    abort.set_stkcmpclr(true);
    let _ = interface.write_raw_dp_register(dp, Abort::ADDRESS, abort.into());
}

/// SWD counterpart of [`jtag_dp_start_with_powerup`].
///
/// Clears any latched sticky error via `ABORT` first (see [`swd_dp_clear_sticky`]) — so
/// a stale error cannot fail the stock `DPIDR` read — then runs the stock power-up,
/// retrying the `ABORT`-clear + power-up once on failure. Callers that need a further
/// fallback (e.g. an XRES reset-acquire) wrap the returned `Err`.
pub(super) fn swd_dp_start_with_powerup(
    interface: &mut dyn DapAccess,
    dp: DpAddress,
) -> Result<(), ArmError> {
    swd_dp_clear_sticky(interface, dp);

    if DefaultArmSequence(())
        .debug_port_start(&mut *interface, dp)
        .is_ok()
    {
        return Ok(());
    }

    swd_dp_clear_sticky(interface, dp);
    DefaultArmSequence(()).debug_port_start(interface, dp)
}

/// Protocol-aware `debug_port_start`: power up the DAP and clear sticky errors the way
/// the active wire protocol requires (CAT1 programming spec).
///
/// - JTAG: [`jtag_dp_start_with_powerup`] — the CTRL/STAT sticky-error bits are
///   write-1-to-clear, so the power-up write clears them. `prime_abort` /
///   `prime_before_first` are forwarded (needed where the SWJ-DP NACKs the stock
///   `DPIDR` read until the DAP is powered up).
/// - SWD: [`swd_dp_start_with_powerup`] — those CTRL/STAT bits are read-only, so sticky
///   errors are cleared through `ABORT` instead. The `prime_*` flags do not apply.
pub(super) fn dp_start_with_powerup(
    interface: &mut dyn DapAccess,
    dp: DpAddress,
    prime_abort: bool,
    prime_before_first: bool,
) -> Result<(), ArmError> {
    let is_jtag = interface.active_wire_protocol() == Some(WireProtocol::Jtag);
    if is_jtag {
        jtag_dp_start_with_powerup(interface, dp, prime_abort, prime_before_first)
    } else {
        swd_dp_start_with_powerup(interface, dp)
    }
}

/// Write one 32-bit word to `addr` through the given AP using raw TAR/DRW register
/// writes.
///
/// `memory_interface()` cannot be used for the SYS AP: that function initialises a
/// standard AMBA memory-AP adapter and resets CSW to a generic default, which clears
/// the SYS AP's `DbgSwEnable` bit and makes subsequent transfers fail. The SYS AP is
/// Infineon's proprietary bus-access portal, not a standard AHB/AXI AP; the CM33/CM55
/// memory APs hit the same problem when their AP IDR reads back a non-MEM-AP value.
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

/// Read one 32-bit word from `addr` through raw AP TAR/DRW accesses.
pub(super) fn read_mem32(
    iface: &mut dyn ArmDebugInterface,
    ap: &FullyQualifiedApAddress,
    addr: u32,
) -> Result<u32, ArmError> {
    iface.write_raw_ap_register(ap, AP_TAR, addr)?;
    iface.read_raw_ap_register(ap, AP_DRW)
}

/// Clear all sticky DP transaction errors through `ABORT`.
pub(super) fn clear_dp_sticky_errors(
    interface: &mut dyn DapAccess,
    dp: DpAddress,
) -> Result<(), ArmError> {
    let mut abort = Abort(0);
    abort.set_orunerrclr(true);
    abort.set_wderrclr(true);
    abort.set_stkerrclr(true);
    abort.set_stkcmpclr(true);
    interface.write_raw_dp_register(dp, Abort::ADDRESS, abort.0)
}

/// Pulse XRES (drive the SWJ nRESET pin low for `hold_ms`, then release) to restart
/// the boot ROM so it enables the SWJ pins.
///
/// Returns `false` only if the probe cannot drive nRESET at all (`swj_pins` errors),
/// in which case no pulse was performed and XRES recovery is unavailable.
pub(super) fn pulse_xres_for(interface: &mut dyn DebugPortWire, hold_ms: u64) -> bool {
    let mut n_reset = Pins(0);
    n_reset.set_nreset(true);

    // Assert XRES (drive nRESET low). Only probes that cannot drive nRESET at all
    // return an error; some probes assert it successfully but return 0xffff_ffff
    // because they cannot read the pin state back, so that value is not a failure.
    if interface
        .swj_pins(Pins(0), n_reset, Duration::ZERO)
        .is_err()
    {
        return false;
    }

    // Hold XRES low, then release it so the boot ROM runs and enables the SWJ pins.
    thread::sleep(Duration::from_millis(hold_ms));
    let _ = interface.swj_pins(n_reset, n_reset, Duration::ZERO);
    true
}

/// Retry `attempt` until it reports success or `timeout` elapses.
///
/// `attempt` returns `Ok(true)` on success (the loop stops and returns `Ok(true)`),
/// `Ok(false)` to keep trying, or `Err(_)` to abort immediately. Between tries the loop
/// sleeps `poll_interval`; pass [`Duration::ZERO`] to tight-hammer a narrow boot-ROM
/// window with no delay. Returns `Ok(false)` if the deadline passes first. `attempt`
/// always runs at least once, even with a zero `timeout`.
///
/// This is the shared skeleton behind the Infineon dormant-wake and reset-acquire retry
/// loops. The per-device work lives in `attempt`.
pub(super) fn retry_until_deadline(
    timeout: Duration,
    poll_interval: Duration,
    mut attempt: impl FnMut() -> Result<bool, ArmError>,
) -> Result<bool, ArmError> {
    let deadline = Instant::now() + timeout;
    loop {
        if attempt()? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        if !poll_interval.is_zero() {
            thread::sleep(poll_interval);
        }
    }
}

/// ARM ADI §B4.3.4 128-bit dormant-to-active selection alert. Protocol-independent:
/// the same alert precedes both the JTAG and the SWD activation code.
const DORMANT_ALERT_LO: u64 = 0x8685_2D95_6209_F392; // alert bits   0..63
const DORMANT_ALERT_HI: u64 = 0x19BC_0EA2_E3DD_AFE9; // alert bits  64..127

/// Which wire protocol the dormant selection alert should activate.
#[derive(Clone, Copy, Debug)]
enum DormantTarget {
    Jtag,
    Swd,
}

/// Leave the currently selected SWJ-DP protocol in the ARM dormant state.
pub(super) fn enter_dormant(interface: &mut dyn DebugPortWire) {
    let Some(protocol) = interface.active_protocol() else {
        return;
    };

    let entered = interface
        .swj_sequence(&BitSequence::from_u64(51, 0x0007_FFFF_FFFF_FFFF))
        .is_ok()
        && match protocol {
            WireProtocol::Swd => interface
                .swj_sequence(&BitSequence::from_u64(16, 0xE3BC))
                .is_ok(),
            WireProtocol::Jtag => interface
                .swj_sequence(&BitSequence::from_u64(31, 0x33BB_BBBA))
                .is_ok(),
        };

    if entered {
        tracing::debug!("Infineon SWJ-DP: entered dormant state from {protocol:?}");
    } else {
        tracing::debug!("Infineon SWJ-DP: failed to enter dormant state from {protocol:?}");
    }
}

/// Send the ARM ADI §B4.3.4 dormant-to-active selection alert to switch the SWJ-DP
/// into `target`.
///
/// The 128-bit selection alert ([`DORMANT_ALERT_LO`]/[`DORMANT_ALERT_HI`]) is shared by
/// both protocols; only the enter-dormant framing, the 8-bit activation code
/// (JTAG `0x0A` / SWD `0x1A`), and the trailer differ. Returns `true` if every sequence
/// was sent — the caller must still confirm the DP actually selected the protocol
/// (JTAG TAP scan / SWD `DPIDR` read).
fn send_dormant_alert(interface: &mut dyn DebugPortWire, target: DormantTarget) -> bool {
    // Line reset, then the protocol-specific "select dormant" framing.
    let line_reset = interface
        .swj_sequence(&BitSequence::from_u64(51, 0x0007_FFFF_FFFF_FFFF))
        .is_ok();
    let protocol_to_dormant = line_reset
        && match target {
            DormantTarget::Jtag => interface
                .swj_sequence(&BitSequence::from_u64(16, 0xE3BC))
                .is_ok(), // SWD → dormant
            DormantTarget::Swd => interface
                .swj_sequence(&BitSequence::from_u64(31, 0x33BB_BBBA))
                .is_ok(), // JTAG → dormant
        };
    let framed = line_reset && protocol_to_dormant;

    // Shared: >=8-cycle preamble + the 128-bit selection alert.
    let preamble = framed
        && interface
            .swj_sequence(&BitSequence::from_u64(8, 0xFF))
            .is_ok();
    let alert_lo = preamble
        && interface
            .swj_sequence(&BitSequence::from_u64(64, DORMANT_ALERT_LO))
            .is_ok();
    let alert_hi = alert_lo
        && interface
            .swj_sequence(&BitSequence::from_u64(64, DORMANT_ALERT_HI))
            .is_ok();
    let alerted = preamble && alert_lo && alert_hi;

    // Protocol-specific 4-low + activation code, then the trailer that lands the DP in
    // its idle state.
    let activated = alerted
        && match target {
            DormantTarget::Jtag => {
                interface.swj_sequence(&BitSequence::from_u64(12, 0x0A0)).is_ok()               // JTAG activation 0x0A
                    && interface.swj_sequence(&BitSequence::from_u64(6, 0x3F)).is_ok()          // Test-Logic-Reset
                    && interface.jtag_sequence(false, &BitSequence::from_u64(1, 0x01)).is_ok() // Run-Test/Idle
            }
            DormantTarget::Swd => {
                interface.swj_sequence(&BitSequence::from_u64(12, 0x1A0)).is_ok()                       // SWD activation 0x1A
                    && interface.swj_sequence(&BitSequence::from_u64(51, 0x0007_FFFF_FFFF_FFFF)).is_ok() // line reset
                    && interface.swj_sequence(&BitSequence::from_u64(3, 0x00)).is_ok() // idle
            }
        };

    tracing::debug!(
        "Infineon SWJ-DP: dormant alert target={target:?} line_reset={line_reset} protocol_to_dormant={protocol_to_dormant} preamble={preamble} alert_lo={alert_lo} alert_hi={alert_hi} activated={activated}"
    );
    activated
}

/// Scan the JTAG chain and, if a TAP is found, configure the probe with the scanned chain.
///
/// Uses the raw IDCODE scan rather than the CMSIS-DAP length scan, which cannot measure
/// chains with a non-bypassing TAP after reset (e.g. the PSOC C3 boundary-scan TAP).
pub(super) fn jtag_tap_visible(interface: &mut dyn DebugPortWire) -> bool {
    let Some(mut chain) = interface.try_jtag_chain() else {
        return interface.configure_jtag(false).is_ok();
    };
    chain.set_chain(&[]);
    let scanned = match chain.scan_chain() {
        Ok(scanned) if !scanned.is_empty() => scanned.to_vec(),
        _ => return false,
    };
    chain.set_expected(&scanned);
    interface.configure_jtag(true).is_ok()
}

/// Send the DORMANT-to-JTAG selection alert and confirm the JTAG TAP is visible.
///
/// Sends the shared selection alert with the JTAG activation code (see
/// [`send_dormant_alert`]), then scans the chain via [`jtag_tap_visible`]; a successful
/// scan (the TAP IDCODE was found) means the DP is now in JTAG mode.
///
/// Returns `Ok(true)` once the TAP becomes visible, or `Ok(false)` if it never does
/// within `timeout`. Pass `None` to use the default [`RESET_FINISH_DELAY_MS`] window;
/// only families whose boot-window differs need to pass an explicit value.
pub(super) fn try_jtag_dormant_wake(
    interface: &mut dyn DebugPortWire,
    timeout: Option<Duration>,
) -> Result<bool, ArmError> {
    let timeout = timeout.unwrap_or(Duration::from_millis(RESET_FINISH_DELAY_MS));
    let mut attempt = 0u32;
    tracing::debug!(
        "Infineon SWJ-DP: starting dormant-to-JTAG wake, timeout={}ms",
        timeout.as_millis()
    );
    let woken = retry_until_deadline(timeout, Duration::from_millis(5), || {
        attempt += 1;
        if send_dormant_alert(interface, DormantTarget::Jtag) {
            if jtag_tap_visible(interface) {
                if let Some(mut chain) = interface.try_jtag_chain()
                    && let Err(error) = chain.select(0)
                {
                    tracing::debug!(
                        "Infineon SWJ-DP: JTAG wake attempt {attempt}: selecting TAP 0 failed: {error}"
                    );
                    return Ok(false);
                }
                tracing::debug!("DORMANT-to-JTAG wake succeeded (JTAG TAP visible)");
                return Ok(true);
            }
            tracing::debug!(
                "Infineon SWJ-DP: JTAG wake attempt {attempt}: post-wake scan found no TAPs"
            );
        } else {
            tracing::debug!("Infineon SWJ-DP: JTAG wake attempt {attempt}: dormant alert failed");
        }
        Ok(false)
    })?;

    if !woken {
        tracing::warn!(
            "DORMANT-to-JTAG wake did not produce a TAP within {}ms ({} attempts)",
            timeout.as_millis(),
            attempt
        );
    }
    Ok(woken)
}

/// Send the DORMANT-to-SWD selection alert and try to read `DPIDR`, retrying until
/// `timeout`.
///
/// Sends the shared selection alert with the SWD activation code (see
/// [`send_dormant_alert`]), then reads `DPIDR` to confirm the DP is up. A failed
/// `DPIDR` read returns the DP to dormant (ARM ADI §B4.3.4), so the whole alert is
/// re-sent before every attempt. Returns `Ok(true)` on the first successful `DPIDR`
/// read, `Ok(false)` on timeout.
pub(super) fn try_swd_dormant_connect(
    interface: &mut dyn DebugPortWire,
    timeout: Duration,
) -> Result<bool, ArmError> {
    let mut attempt = 0u32;
    let connected = retry_until_deadline(timeout, Duration::from_millis(5), || {
        attempt += 1;
        if send_dormant_alert(interface, DormantTarget::Swd) {
            match interface.raw_read_register(
                Port::Dp,
                RegisterAddress::DpRegister(DPIDR::ADDRESS).a2_and_3(),
            ) {
                Ok(v) => {
                    tracing::debug!("DORMANT-to-SWD connect: attempt {attempt}: DPIDR=0x{v:08X}");
                    return Ok(true);
                }
                Err(e) => {
                    tracing::trace!("DORMANT-to-SWD connect: attempt {attempt}: DPIDR NACK: {e}");
                }
            }
        }
        Ok(false)
    })?;

    if !connected {
        tracing::warn!("DORMANT-to-SWD connect timed out after {attempt} attempts");
    }
    Ok(connected)
}

/// Wake the dormant SWJ-DP into JTAG, then connect; on failure fall back to the default
/// setup so a real error surfaces.
///
/// Shared tail of the Infineon JTAG `debug_port_setup` paths: on a successful wake the
/// JTAG `debug_port_connect` is a no-op scan-chain program, so it is called directly;
/// otherwise the default sequence is retried (it re-attempts the non-dormant switch and
/// reports a proper error). `timeout` bounds the wake retry loop (`None` → the default
/// [`RESET_FINISH_DELAY_MS`] boot-window budget).
pub(super) fn jtag_dormant_wake_or_default(
    interface: &mut dyn DebugPortWire,
    dp: DpAddress,
    timeout: Option<Duration>,
) -> Result<(), ArmError> {
    if try_jtag_dormant_wake(interface, timeout)? {
        return DefaultArmSequence(()).debug_port_connect(interface, dp);
    }
    DefaultArmSequence(()).debug_port_setup(interface, dp)
}

/// Wake the dormant SWJ-DP into SWD (`JTAG_to_DORMANT` framing + selection alert +
/// `DORMANT_to_SWD` activation), then connect; on failure fall back to the default setup
/// so a real error surfaces.
///
/// SWD counterpart of [`jtag_dormant_wake_or_default`]. The Infineon SWJ-DP powers up in
/// the ARM dormant state, so the stock JTAG-to-SWD switch (`0xE79E`) sent by
/// [`DefaultArmSequence::debug_port_setup`] does not select SWD; the default only reaches
/// the dormant alert after two failed non-dormant attempts. This sends the dormant switch
/// up front via [`try_swd_dormant_connect`] and, on a confirmed `DPIDR` read, runs the
/// SWD `debug_port_connect`. `timeout` bounds the wake retry loop (`None` → the default
/// [`RESET_FINISH_DELAY_MS`] boot-window budget).
pub(super) fn swd_dormant_connect_or_default(
    interface: &mut dyn DebugPortWire,
    dp: DpAddress,
    timeout: Option<Duration>,
) -> Result<(), ArmError> {
    let timeout = timeout.unwrap_or(Duration::from_millis(RESET_FINISH_DELAY_MS));
    if try_swd_dormant_connect(interface, timeout)? {
        return DefaultArmSequence(()).debug_port_connect(interface, dp);
    }
    DefaultArmSequence(()).debug_port_setup(interface, dp)
}

/// Clear the DP sticky-error flags and abort any pending AP transaction via `ABORT`.
///
/// Used to recover the DP after a failed AHB access so the next AP access succeeds.
/// Best-effort — logs the outcome and swallows the error, since the caller has no
/// fallback.
pub(super) fn recover_dp(interface: &mut dyn DapAccess, dp: DpAddress) {
    let mut abort = Abort(0);
    abort.set_dapabort(true);
    abort.set_orunerrclr(true);
    abort.set_wderrclr(true);
    abort.set_stkerrclr(true);
    abort.set_stkcmpclr(true);
    match interface.write_raw_dp_register(dp, Abort::ADDRESS, abort.0) {
        Ok(()) => tracing::debug!("Cleared DP sticky error flags"),
        Err(e) => tracing::warn!(
            "Failed to clear DP sticky error flags: {:?} (potential debug transport issue)",
            e
        ),
    }
}

/// `AIRCR.SYSRESETREQ` reset for targets whose system reset also resets the debug port.
pub(super) fn cortex_m_reset_system_with_recovery(
    interface: &mut dyn ArmMemoryInterface,
    recover: impl FnMut(&mut dyn ArmMemoryInterface) -> Result<(), ArmError>,
) -> Result<(), ArmError> {
    let mut aircr = Aircr(0);
    aircr.vectkey();
    aircr.set_sysresetreq(true);

    if let Err(err) = interface.write_word_32(Aircr::get_mmio_address(), aircr.into()) {
        // The reset can drop the debug port before the AIRCR write is acknowledged.
        if !is_no_acknowledge(&err) {
            return Err(err);
        }
        tracing::debug!("AIRCR write was not acknowledged, assuming the target reset: {err}");
    }

    cortex_m_wait_for_reset_with_recovery(interface, recover)
}

fn is_no_acknowledge(err: &ArmError) -> bool {
    let mut cause: &(dyn Error + 'static) = err;
    loop {
        if matches!(
            cause.downcast_ref::<ArmError>(),
            Some(ArmError::Dap(DapError::NoAcknowledge))
        ) {
            return true;
        }
        let Some(next) = cause.source() else {
            return false;
        };
        cause = next;
    }
}

/// Wait for the core to leave reset, reconnecting via `recover` when the debug port
/// stops acknowledging.
pub(super) fn cortex_m_wait_for_reset_with_recovery(
    interface: &mut dyn ArmMemoryInterface,
    mut recover: impl FnMut(&mut dyn ArmMemoryInterface) -> Result<(), ArmError>,
) -> Result<(), ArmError> {
    let start = Instant::now();

    // PSOC 6 documentation states 600ms is the maximum possible time
    // before the debug port becomes available again after reset
    while start.elapsed() < Duration::from_millis(600) {
        let dhcsr = match interface.read_word_32(Dhcsr::get_mmio_address()) {
            Ok(val) => Dhcsr(val),
            Err(err) => {
                if is_no_acknowledge(&err) {
                    if let Ok(probe) = interface.get_arm_debug_interface() {
                        // Let the reset propagate before reopening the debug connection.
                        thread::sleep(Duration::from_millis(100));
                        probe.reinitialize()?;
                        recover(interface)?;
                    }
                    continue;
                }

                return Err(err);
            }
        };
        if !dhcsr.s_reset_st() {
            return Ok(());
        }
    }

    Err(ArmError::Timeout)
}
