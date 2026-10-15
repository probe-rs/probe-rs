//! Sequences for NSING N32H7xx series devices.
//!
//! This silicon needs help around a system reset. Writing `SYSRESETREQ` to AIRCR
//! stops the AHB-AP answering for as long as the reset is in flight, and the
//! write itself reports a FAULT while that happens. The FAULT is *expected* —
//! the vendor's own CMSIS-Pack wraps the very same write in error suppression
//! ("the write-readback fails because the reset fires immediately after the
//! write") — but the generic `cortex_m_wait_for_reset` only re-establishes the
//! debug connection on `NoAcknowledge`, never on FAULT. Nothing recovers the
//! connection, and the rest of the procedure then runs against a dead AP.
//!
//! So this sequence follows the ordering of the vendor's own working J-Link
//! script (`N32H7xx_CM7.jlinkscript`, `ResetTarget()`): clear the DP's sticky
//! errors, clear `DEMCR.VC_CORERESET`, issue the reset, give it time to complete
//! before touching the AP again, then re-establish the connection if it is not
//! answering.
//!
//! That second step is not optional, and it is the one the generic path gets
//! wrong: `reset_and_halt` arms `VC_CORERESET` *before* calling a sequence, and
//! writing `SYSRESETREQ` with it armed leaves the AHB-AP dead for good. Vector
//! catch halts the core the instant it leaves reset — inside the boot window,
//! during which this silicon has SWD disabled — so boot never finishes and the
//! debug port is never un-gated. SEGGER calls the setting out by name, and
//! `probe-rs reset`, which never arms it, survives the identical write.
//!
//! If the AP never comes back the sequence degrades to leaving the core halted
//! where it is, which is still enough to flash — a board that is running an
//! application with a usable debug port needs no more than that.
//!
//! # The dual-core 78x parts
//!
//! The vendor's naming splits three ways: 73x and 76x are single-core Cortex-M7,
//! 78x adds a Cortex-M4 on its own AHB-AP (`__ap="2"` in the vendor's PDSC). The
//! M4 is *gated* by `RCC->M4RSTREL`, and that gate drives the debug port itself:
//! measured on an N32H785, AP2 answers only while the M4 is released. With
//! `M4RSTREL` at its reset value of 0 — every part that has just been powered up,
//! and every part after `SYSRESETREQ` — AP2 does not answer at all.
//!
//! That matters before anything else does. `Session::new` attaches *every* core
//! the chip description declares, and gives up on the whole session if one of
//! them cannot be reached, so a dual-core description against an un-released M4
//! fails to connect even to the M7. `debug_core_start` therefore skips the second
//! core while a session is being built, and the release happens in `on_attach` —
//! when something actually asks for the M4.
//!
//! Releasing it any earlier than that is not harmless, it is what used to break
//! M7 flashing. A released core starts fetching from `MMU->M4_BOOT_ADDR` on the
//! next clock, and on a part whose Bank2 has never been programmed that reads as
//! `0xFFFFFFFF`: the M4 faults and the M7 goes down with it. Measured on an
//! N32H785, the M7 dies inside the flash algorithm's ROM call with
//! `CFSR=0x401` (IACCVIOL + IMPRECISERR), `HFSR=0x40000002` (FORCED + VECTTBL)
//! and `DHCSR=0x00090001` (`S_LOCKUP=1`) — the fault cannot even be vectored,
//! because `VTOR` is still 0. The same probe, the same binary and a chip
//! description that simply has no second core flash that part perfectly.
//!
//! The recipe is the vendor's own `RCC_EnableCM4()` from `n32h7xx_rcc.c`, and it
//! is two steps, not one: point `MMU->M4_BOOT_ADDR` at the M4's vector table
//! *first*, then set `M4RSTREL`. Their PDSC sequence (`releaseM4AndHaltM7`) only
//! does the second half, and on a part straight out of power-on reset that leaves
//! the M4 fetching vectors from address 0.
//!
//! Resetting the M4 is its own story. The whole-SoC reset gates the core again,
//! and the FPB unit sits in the same gated domain: measured, an M4 that has just
//! come back holds nothing but zeros in its comparators. That is fatal to the
//! one flow that resets a core mid-session — the debugger's `restart` — where
//! the breakpoints the client set have to keep working afterwards; they are
//! captured before the reset and put back after it (`snapshot_secondary_fpb`).
//!
//! The same gate means the M4's entry breakpoint cannot be armed the way the
//! M7's is — before the reset, to survive it. The M4's is armed after, on the
//! unit the reset left empty, and the core is then sent through a
//! *processor-local* `VECTRESET`: it starts fetching from its vector table
//! again without the debug domain going down with it, and the armed comparator
//! catches it on its very first instruction. A `restart` therefore leaves the
//! M4 stopped at its entry with the client's breakpoints already back in the
//! unit, and one on the one-shot start-up part of an application fires again —
//! the way it already does on the M7. Merely halting the released core instead
//! is not enough and is what this replaces: measured, the core reaches its main
//! loop before that halt lands.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use probe_rs_target::{Chip, CoreType};

use crate::{
    MemoryMappedRegister,
    architecture::arm::{
        ArmDebugInterface, ArmError, DapAccess, FullyQualifiedApAddress,
        ap::{ApRegister, CSW, DRW, IDR, TAR},
        armv7m::{Aircr, Demcr, Dhcsr},
        dp::{Abort, DpAccess, DpAddress, DpRegister},
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, DefaultArmSequence},
    },
    config::CoreExt,
};

/// The vendor's naming: N32H7xx covers both the single-core 73x/76x parts and the
/// dual-core 78x parts.
const CPUID: u64 = 0xE000_ED00;
/// What a Cortex-M7 r1p2 reports. Used to tell "the AP is answering" apart from
/// "the AP is answering *correctly*".
const EXPECTED_CPUID: u32 = 0x411F_C272;
/// What the 78x parts' Cortex-M4 reports — measured, not assumed.
const EXPECTED_CPUID_M4: u32 = 0x410F_C241;

/// `RCC->M4RSTREL`: bit 0 releases the M4 from reset. Reset value is 0, so the M4
/// is gated off until something releases it.
const M4RSTREL: u64 = 0x5803_0174;
/// `M4RSTREL` bit 0: `RCC_M4RSTREL_EN` in the vendor's CMSIS device header,
/// `RCC_RSTEN_M4REL` in the RCC driver header.
const M4RSTREL_M4REL: u32 = 1 << 0;
/// `MMU->M4_BOOT_ADDR`: where the boot logic maps the M4's vector table from.
///
/// Deliberately addressed through the *M7's* AP: the vendor's `RCC_EnableCM4()`
/// writes it from the M7's view of memory, and so does their PDSC sequence.
const M4_BOOT_ADDR: u64 = 0x5110_50AC;
/// The M4's own flash bank on every dual-core part, and the address the vendor's
/// own dual-core examples pass to `RCC_EnableCM4()`. Used only when
/// `M4_BOOT_ADDR` reads as something that cannot hold a vector table.
const M4_BOOT_ADDR_DEFAULT: u32 = 0x1508_0000;
/// How long to give AP2 to answer after the M4 is released.
const M4_RELEASE_TIMEOUT: Duration = Duration::from_millis(200);

/// A `CSW` that makes TAR/DRW transfers word-sized: `Size=U32` (bits 2:0),
/// `AddrInc=Single` (bits 5:4) and the control bits the AMBA-AHB5 bring-up
/// sets. Measured — a plain probe-rs session leaves exactly this value on both
/// APs of an N32H785.
const CSW_WORD_ACCESS: u32 = 0x2300_0052;

/// How long to let a reset complete before touching the AP again. SEGGER's
/// script sleeps 600 ms at this point, the vendor's CMSIS-Pack 500 ms.
const RESET_SETTLE: Duration = Duration::from_millis(600);
/// Total budget for getting the AP back on its feet after the reset.
const RECOVERY_TIMEOUT: Duration = Duration::from_millis(1000);
/// Total budget for a halt request to be acknowledged, once debug is enabled.
const HALT_TIMEOUT: Duration = Duration::from_millis(200);
/// Total budget for a processor-local reset (`VECTRESET`) to be seen through,
/// measured by `DHCSR.S_RESET_ST` clearing. The reset itself is instantaneous;
/// the budget only covers the reads that clear the sticky bit.
const VECTRESET_TIMEOUT: Duration = Duration::from_millis(100);

/// `MMU->M7_StoreAddr_cur`: where the boot logic has the M7's code mapped. The
/// vendor's script reads the application's reset vector through it. The address
/// is the same in the 73x, 76x and 78x `ResetSystem` sequences.
const M7_STORE_ADDR_CUR: u64 = 0x5110_511C;
/// Flash Patch and Breakpoint unit control. Bit 0 enables the unit, bit 1 is the
/// KEY that must accompany every write to it.
const FP_CTRL: u64 = 0xE000_2000;
/// FPB comparator 0: the address in bits 31:2, its own enable in bit 0.
const FP_COMP0: u64 = 0xE000_2008;

/// Initialization sequencing for N32H7xx parts.
#[derive(Debug)]
pub struct N32H7xx {
    /// The AP of the core the chip description lists first: the Cortex-M7, on
    /// both the single- and the dual-core parts.
    primary_ap: FullyQualifiedApAddress,
    /// The AP of the second core, when the part has one.
    ///
    /// `None` on the single-core 73x/76x parts, which turns every dual-core path
    /// below into a no-op — that is what keeps their behaviour unchanged.
    secondary_ap: Option<FullyQualifiedApAddress>,
    /// Whether an attach in this session has already normalized the second
    /// core's FPB. One `N32H7xx` is built per session (`Target::new`), so this
    /// is a per-session latch. See `normalize_secondary_fpb_if_due`.
    secondary_fpb_sanitized: AtomicBool,
}

/// Where `arm_entry_breakpoint` put the entry breakpoint, and what it had to
/// change for the unit to match on it: everything `disarm_entry_breakpoint`
/// needs to put the FPB back the way it found it.
#[derive(Debug)]
struct ArmedEntryBreakpoint {
    /// Which comparator holds the entry breakpoint.
    index: u32,
    /// Whether the unit had ENABLE set before arming. Disarming restores this
    /// rather than switching the unit off, because the other comparators may
    /// hold breakpoints that are not ours.
    was_enabled: bool,
}

/// The second core's FPB, as it was just before that core was reset.
///
/// The unit sits in the domain `RCC->M4RSTREL` gates, so a reset of the core
/// leaves the comparators holding fresh hardware state and everything the
/// session had armed on it is gone. Capturing it is what makes putting it back
/// possible.
#[derive(Debug)]
struct FpbSnapshot {
    /// Whether `FP_CTRL` had ENABLE set; the rest of the register is read-only
    /// configuration.
    enabled: bool,
    /// Every comparator's raw value, `FP_COMP0` first.
    comparators: Vec<u32>,
}

impl N32H7xx {
    /// Create the sequencer for the N32H7xx series.
    ///
    /// Reads the core layout out of the chip description rather than hard-coding
    /// it, so that the single-core 73x/76x parts and the dual-core 78x parts run
    /// the same code with the dual-core half simply absent. Cores are told apart
    /// by their *AP*, not by name, because the AP number is what the vendor's
    /// PDSC actually pins down (`__ap="0"` / `__ap="2"`) while core names are
    /// free-form.
    pub fn create_for_chip(chip: &Chip) -> Arc<Self> {
        let mut aps = chip.cores.iter().filter_map(CoreExt::memory_ap);

        let primary_ap = aps.next().unwrap_or_else(|| {
            // A chip with no ARM memory AP is not something we can sequence, but
            // the vendor match is by name, so be explicit rather than panic.
            FullyQualifiedApAddress::v1_with_dp(DpAddress::Default, 0)
        });

        let secondary_ap = aps.find(|ap| *ap != primary_ap);

        Arc::new(Self {
            primary_ap,
            secondary_ap,
            secondary_fpb_sanitized: AtomicBool::new(false),
        })
    }

    /// Which CPUID this AP's core should report.
    ///
    /// `None` for an AP this sequence does not know about, which callers treat as
    /// "any sane Cortex-M will do" rather than failing the reset outright.
    fn expected_cpuid(&self, ap: &FullyQualifiedApAddress) -> Option<u32> {
        if self.secondary_ap.as_ref() == Some(ap) {
            Some(EXPECTED_CPUID_M4)
        } else if *ap == self.primary_ap {
            Some(EXPECTED_CPUID)
        } else {
            None
        }
    }

    /// Make sure this AP's transfers are word-sized.
    ///
    /// `CSW.Size` is persistent hardware state, shared by everything that talks
    /// to the AP, and probe-rs only rewrites it lazily — when a transfer of a
    /// different size happens to pass through the session's cached `MemoryAp`.
    /// An AP whose last transfer was an 8- or 16-bit one (a flash algorithm's
    /// stack-fill check reads exactly one byte) therefore still has that size in
    /// the next session, and the raw paths here assume a word: an 8-bit read of
    /// `CPUID` returns one real byte followed by whatever `DRW` last held, which
    /// reads as "this is not the expected core" even though the AP is perfectly
    /// healthy. Measured: with `CSW=0x23000050` the M4's `CPUID` read back as a
    /// truncated value and the session refused to attach with "Core 1 is not
    /// enabled"; writing the value below is what made it answer again.
    ///
    /// Only the size field is inspected; an AP that is already word-sized is
    /// left exactly as found.
    fn ensure_word_access<D: DapAccess + ?Sized>(
        interface: &mut D,
        ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let csw = interface.read_raw_ap_register(ap, CSW::ADDRESS)?;
        if (csw & 0b111) == (CSW_WORD_ACCESS & 0b111) {
            return Ok(());
        }

        tracing::info!(
            "N32H7xx: AP {ap:?} has CSW=0x{csw:08X}, which does not transfer words; \
             writing 0x{CSW_WORD_ACCESS:08X}"
        );
        interface.write_raw_ap_register(ap, CSW::ADDRESS, CSW_WORD_ACCESS)?;

        let readback = interface.read_raw_ap_register(ap, CSW::ADDRESS)?;
        if (readback & 0b111) != (CSW_WORD_ACCESS & 0b111) {
            tracing::warn!(
                "N32H7xx: AP {ap:?} still reads CSW=0x{readback:08X} after the write; \
                 word-sized access through it cannot be trusted"
            );
            return Err(ArmError::Timeout);
        }
        Ok(())
    }

    /// Read a word straight through the AP registers.
    ///
    /// Deliberately bypasses `ArmMemoryInterface::read_word_32`, which would go
    /// through the CSW shadow cached in `MemoryAp` — a cache a reset may well
    /// have invalidated. The raw path has no such cache, so it asserts the
    /// word-sized `CSW` itself first.
    ///
    /// Generic over the interface rather than taking `&mut dyn ArmDebugInterface`
    /// because the M4 release is driven from hooks that only hand out a bare
    /// `&mut dyn DapAccess`.
    fn read_word<D: DapAccess + ?Sized>(
        interface: &mut D,
        ap: &FullyQualifiedApAddress,
        address: u64,
    ) -> Result<u32, ArmError> {
        Self::ensure_word_access(interface, ap)?;
        interface.write_raw_ap_register(ap, TAR::ADDRESS, address as u32)?;
        interface.read_raw_ap_register(ap, DRW::ADDRESS)
    }

    /// Write a word straight through the AP registers, for the same reason.
    fn write_word<D: DapAccess + ?Sized>(
        interface: &mut D,
        ap: &FullyQualifiedApAddress,
        address: u64,
        value: u32,
    ) -> Result<(), ArmError> {
        Self::ensure_word_access(interface, ap)?;
        interface.write_raw_ap_register(ap, TAR::ADDRESS, address as u32)?;
        interface.write_raw_ap_register(ap, DRW::ADDRESS, value)
    }

    /// Clear the DP's sticky error bits through the raw interface.
    ///
    /// On SWD a single faulted AP access blocks every access after it until ABORT
    /// clears the error, so a scan that expects some APs to fault has to do this
    /// between attempts.
    fn clear_sticky_errors<D: DapAccess + ?Sized>(interface: &mut D, dp: DpAddress) {
        let mut abort = Abort(0);
        abort.set_orunerrclr(true);
        abort.set_wderrclr(true);
        abort.set_stkerrclr(true);
        abort.set_stkcmpclr(true);

        if let Err(e) = interface.write_raw_dp_register(dp, Abort::ADDRESS, u32::from(abort)) {
            tracing::warn!("N32H7xx: could not clear the DP's sticky errors: {e}");
        }
    }

    /// Offer the second core a sensible vector-table address, if it does not
    /// already have one.
    ///
    /// The vendor's `RCC_EnableCM4()` sets this *before* releasing the core. Their
    /// PDSC sequence skips it, which is harmless only when something else has
    /// already written the register — the M7's own firmware does that on the
    /// vendor's dual-core examples, but nothing does on a part that has just been
    /// powered up, where the register reads 0 and a release would leave the M4
    /// fetching vectors from address 0.
    fn point_m4_at_its_vector_table<D: DapAccess + ?Sized>(
        iface: &mut D,
        ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let boot_addr = Self::read_word(iface, ap, M4_BOOT_ADDR)?;

        // Value already there wins: the running firmware knows better than we do
        // where its own vector table is. Only a value that cannot hold one is
        // replaced, which is the same shape of check `arm_entry_breakpoint` uses.
        if boot_addr != 0 && boot_addr % 4 == 0 {
            tracing::debug!(
                "N32H7xx: MMU->M4_BOOT_ADDR already reads 0x{boot_addr:08X}; leaving it alone"
            );
            return Ok(());
        }

        tracing::info!(
            "N32H7xx: MMU->M4_BOOT_ADDR reads 0x{boot_addr:08X}, which cannot hold a \
             vector table; setting it to 0x{M4_BOOT_ADDR_DEFAULT:08X} as the vendor's \
             RCC_EnableCM4() does"
        );
        Self::write_word(iface, ap, M4_BOOT_ADDR, M4_BOOT_ADDR_DEFAULT)
    }

    /// Release the second core from reset, the way the vendor's `RCC_EnableCM4()`
    /// does it, and wait for its AP to start answering.
    ///
    /// A no-op when the chip description has only one core, so the single-core
    /// 73x/76x parts are untouched by all of this.
    ///
    /// This is deliberately *only* a release — no halt of the primary core. The
    /// vendor's PDSC sequence pairs the release with `HaltM7` for M4 *downloads*,
    /// to keep the M7 from disturbing them through its DMA; doing it here would
    /// stop the M7 as a side effect of merely connecting to debug the M4, which
    /// is not ours to decide. The reset path, which knows what the core is about
    /// to be used for, adds that half itself.
    /// Returns whether *this* call is the one that let the core out. That matters
    /// to the caller: a core that was already running may have been running for a
    /// long time and does not want to be halted out of the blue, while one we just
    /// released is about to fetch its first instruction and really does.
    fn release_secondary_core<D: DapAccess + ?Sized>(
        &self,
        iface: &mut D,
    ) -> Result<bool, ArmError> {
        let Some(secondary_ap) = self.secondary_ap.as_ref() else {
            return Ok(false);
        };

        // Everything is driven through the *primary* core's AP: the M7 owns these
        // registers in the vendor's own flow, and driving them from the M4's AP
        // would depend on the very core whose power state we are trying to change.
        let ap = self.primary_ap.clone();

        Self::point_m4_at_its_vector_table(iface, &ap)?;

        let mut released_now = false;
        let rstrel = Self::read_word(iface, &ap, M4RSTREL)?;
        if rstrel & M4RSTREL_M4REL != 0 {
            tracing::debug!(
                "N32H7xx: RCC->M4RSTREL already reads 0x{rstrel:08X}; the second core is \
                 already released"
            );
        } else {
            tracing::info!(
                "N32H7xx: releasing the second core (RCC->M4RSTREL 0x{rstrel:08X} -> \
                 0x{:08X})",
                rstrel | M4RSTREL_M4REL
            );
            Self::write_word(iface, &ap, M4RSTREL, rstrel | M4RSTREL_M4REL)?;

            let readback = Self::read_word(iface, &ap, M4RSTREL)?;
            if readback & M4RSTREL_M4REL == 0 {
                tracing::warn!(
                    "N32H7xx: RCC->M4RSTREL reads back 0x{readback:08X}; the second core \
                     was not released"
                );
                return Err(ArmError::Timeout);
            }
            released_now = true;
        }

        // The M4's AP only starts answering once the core is out of reset, so give
        // it a moment before anyone tries to attach to it. Without this the first
        // access races the release and reads as "this core does not exist".
        let deadline = Instant::now() + M4_RELEASE_TIMEOUT;
        loop {
            match iface.read_raw_ap_register(secondary_ap, IDR::ADDRESS) {
                Ok(idr) if idr != 0 => {
                    tracing::debug!("N32H7xx: the second core's AP is up (IDR=0x{idr:08X})");
                    return Ok(released_now);
                }
                Ok(idr) => tracing::debug!(
                    "N32H7xx: the second core's AP still reads IDR=0x{idr:08X} after the \
                     release"
                ),
                Err(e) => {
                    // A faulted AP access leaves STICKYERR set on SWD, which would
                    // block every later access too, so clear it before retrying.
                    tracing::debug!("N32H7xx: the second core's AP is not answering yet: {e}");
                    Self::clear_sticky_errors(iface, ap.dp());
                }
            }

            if Instant::now() >= deadline {
                // Not fatal: the primary core is still perfectly usable, and a
                // session that does not mention the second core does not care.
                tracing::warn!(
                    "N32H7xx: the second core's AP did not come up within {:?} of the \
                     release; a session that attaches to it will fail",
                    M4_RELEASE_TIMEOUT
                );
                return Ok(released_now);
            }

            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Re-release the second core after a reset of that core, and report whether
    /// its AP may be used again.
    ///
    /// A whole-SoC reset gates the core and takes its AP with it, so the release
    /// has to be redone before *anything* may be asked of that AP. Asking a gated
    /// AP for a memory read is not an access that fails: measured on a probe that
    /// does not retry a refused transfer internally (ULINK2), the `TAR` write
    /// goes out, the `DRW` read is never answered at all, the probe waits out its
    /// full USB timeout on it, and the DP operations queued behind it fail too.
    /// The release itself drives the primary core's AP — the one that is back by
    /// then — and is idempotent, so a refusal is retried rather than given up on.
    fn rerelease_secondary_core(&self, arm: &mut dyn ArmDebugInterface) -> bool {
        let Some(secondary_ap) = self.secondary_ap.as_ref() else {
            return false;
        };

        let deadline = Instant::now() + RECOVERY_TIMEOUT;
        loop {
            match self.release_secondary_core(arm) {
                Ok(released_now) => {
                    if released_now {
                        tracing::debug!("N32H7xx: re-released the second core after its reset");
                    }
                    break;
                }
                Err(e) if Instant::now() >= deadline => {
                    // The core may never have been gated in the first place, and
                    // the primary core is usable either way; what must not happen
                    // next is another access through the AP that never came up.
                    tracing::warn!(
                        "N32H7xx: could not re-release the second core: {e}; leaving its AP \
                         alone instead of asking a gated one for anything else"
                    );
                    return false;
                }
                Err(e) => {
                    tracing::debug!("N32H7xx: re-releasing the second core failed: {e}");
                    // A refused AP access leaves STICKYERR set, which would block
                    // every later access through this DP as well, this retry
                    // included.
                    Self::clear_sticky_errors(arm, self.primary_ap.dp());
                    thread::sleep(Duration::from_millis(25));
                }
            }
        }

        // The IDR read `release_secondary_core` waits for only says the AP is
        // powered; that a *memory* access through it lands is the stronger claim
        // everything after this depends on. Same check, same reason, as the one
        // `on_attach` gates the second core's session on.
        self.ap_is_alive(arm, secondary_ap)
    }

    /// Is the AP answering, and answering sanely?
    ///
    /// Exercises a DRW *write* as well as reads, because a DRW write is the
    /// operation that reports the FAULT while a reset is in flight — an AP that
    /// can only be read from is not one we can flash through.
    fn ap_is_alive(&self, arm: &mut dyn ArmDebugInterface, ap: &FullyQualifiedApAddress) -> bool {
        let cpuid = match Self::read_word(arm, ap, CPUID) {
            Ok(cpuid) => cpuid,
            Err(e) => {
                // Kept at debug: the AP being mute is the normal state right
                // after a reset, and the recovery loop asks repeatedly.
                tracing::debug!("N32H7xx: AP {ap:?} did not answer a CPUID read: {e}");
                return false;
            }
        };

        match self.expected_cpuid(ap) {
            // Each core is checked against its *own* part number: the M7 and the
            // M4 report different ones, so a single constant would call every
            // perfectly healthy M4 a dead AP.
            Some(expected) if cpuid != expected => {
                // The AP answered but with the wrong value — worth a warning,
                // because this is precisely the signature of a truncated read
                // (a `CSW` left at 8 bits), not of a dead AP.
                tracing::warn!(
                    "N32H7xx: AP {ap:?} reports CPUID 0x{cpuid:08X}, not the expected \
                     0x{expected:08X}; treating the AP as dead"
                );
                return false;
            }
            // An AP we do not have a description for: accept anything a
            // Cortex-M would report, so an unexpected chip description
            // degrades to "probably fine" rather than "never alive".
            None if cpuid >> 24 != 0x41 || cpuid & 0x000F_FFF0 == 0 => {
                tracing::debug!(
                    "N32H7xx: AP {:?} reports a CPUID of 0x{cpuid:08X}, which does not \
                     look like a Cortex-M; treating the AP as dead",
                    ap
                );
                return false;
            }
            _ => {}
        }

        // DEMCR is known writable here and this sequence owns its contents, so
        // writing the value straight back is a safe way to prove writes land.
        let demcr = match Self::read_word(arm, ap, Demcr::get_mmio_address()) {
            Ok(value) => value,
            Err(e) => {
                tracing::debug!("N32H7xx: AP {ap:?} did not answer a DEMCR read: {e}");
                return false;
            }
        };
        match Self::write_word(arm, ap, Demcr::get_mmio_address(), demcr) {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!("N32H7xx: AP {ap:?} would not take a DEMCR write-back: {e}");
                false
            }
        }
    }

    /// Clear the DP's sticky error bits. SEGGER calls this immediately before
    /// writing AIRCR, and a reset is a good way to leave them set.
    fn clear_dp_errors(&self, arm: &mut dyn ArmDebugInterface, ap: &FullyQualifiedApAddress) {
        let mut abort = Abort(0);
        abort.set_orunerrclr(true);
        abort.set_wderrclr(true);
        abort.set_stkerrclr(true);
        abort.set_stkcmpclr(true);

        if let Err(e) = arm.write_dp_register(ap.dp(), abort) {
            tracing::debug!("N32H7xx: could not clear DP sticky errors: {e}");
        }
    }

    /// Halt the core and leave debug enabled (`DHCSR = 0xA05F0003`).
    fn halt_core(
        &self,
        arm: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let mut dhcsr = Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_debugen(true);
        dhcsr.set_c_halt(true);

        Self::write_word(arm, ap, Dhcsr::get_mmio_address(), dhcsr.into())
    }

    /// Halt the core and confirm that it took.
    ///
    /// A halt request that is never acknowledged would otherwise only surface
    /// much later, as a timeout in `reset_and_halt`'s `wait_for_core_halted` —
    /// far from the reset that caused it, and with a power cycle as the price of
    /// finding out. Asking until `S_HALT` says so keeps the failure here.
    fn halt_and_confirm(
        &self,
        arm: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let deadline = Instant::now() + HALT_TIMEOUT;
        let mut asked = false;

        loop {
            let dhcsr = Dhcsr(Self::read_word(arm, ap, Dhcsr::get_mmio_address())?);
            if dhcsr.s_halt() {
                return Ok(());
            }

            // Which of the two stopped the core is worth knowing: the entry
            // breakpoint halts it at the application's first instruction,
            // whereas a HALT request can only land an unknown time into the
            // running application. Logged once, on the first pass.
            if !asked {
                tracing::debug!("N32H7xx: the core is running, asking it to halt");
                asked = true;
            }

            self.halt_core(arm, ap)?;

            if Instant::now() >= deadline {
                return Err(ArmError::Timeout);
            }

            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Restart a halted core through a processor-local reset (`AIRCR.VECTRESET`).
    ///
    /// The vendor's own `ResetProcessor` sequence, precondition included — their
    /// comment says a `VECTRESET` does not take effect unless the core is
    /// halted, and the ARM ARM says the same in the register description: the
    /// effect of the bit is UNPREDICTABLE unless the processor is in Debug
    /// state.
    ///
    /// What this buys over `RCC->M4RSTREL`, the other way to restart just this
    /// core, is the debug domain: a processor reset leaves it alone, so an entry
    /// breakpoint armed just before the write is still in the unit when the core
    /// starts fetching from its vector table — and catches it on the first
    /// instruction of the application. Taking the core through its gate again
    /// would take the FPB down with it, which is the whole problem here.
    ///
    /// `DHCSR.S_RESET_ST` is waited out the way the vendor does it: the bit is
    /// sticky, set by a reset and cleared by a read of `DHCSR`.
    fn vectreset(
        &self,
        arm: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_vectreset(true);

        Self::write_word(arm, ap, Aircr::get_mmio_address(), aircr.into())?;

        let deadline = Instant::now() + VECTRESET_TIMEOUT;
        loop {
            let dhcsr = Dhcsr(Self::read_word(arm, ap, Dhcsr::get_mmio_address())?);
            if !dhcsr.s_reset_st() {
                return Ok(());
            }

            if Instant::now() >= deadline {
                return Err(ArmError::Timeout);
            }

            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Halt the core at the application's entry point, without vector catch.
    ///
    /// This is the vendor's own substitute for `DEMCR.VC_CORERESET`, and it is
    /// the same in all three families' `ResetSystem` sequences: arm the FPB unit
    /// with a code breakpoint on the application's reset vector. Vector catch
    /// fires the instant the core leaves reset; this fires once the core has
    /// actually reached the application, which is the halt SEGGER describes.
    ///
    /// Best-effort throughout — the explicit halt afterwards is what guarantees
    /// a stopped core, so anything that fails here is logged and skipped.
    ///
    /// The vector is found through the boot-address register *of the core being
    /// armed*, which is the one thing the vendor's per-core `ResetSystem`
    /// sequences do differently: `M7_StoreAddr_cur` in `ResetSystem_M7`,
    /// `M4_BOOT_ADDR` in `ResetSystem_M4`. Reading the M7's register for both —
    /// as this used to — arms the M4's comparator at the *M7's* entry, an address
    /// the M4 can never reach: the breakpoint never fires, and the M4 runs its
    /// application until the explicit halt at the end lands somewhere inside it.
    ///
    /// The comparator it goes into is the first one that reads exactly zero, so
    /// the session's own breakpoints — which `set_hw_breakpoint` places from
    /// index 0 upwards — are left alone. Arming in comparator 0 unconditionally
    /// and clearing comparator 0 on the way out is what silently deleted the
    /// first user breakpoint of every session that went through a
    /// reset-and-halt, measured across a debugger `restart`: `FP_COMP0` went
    /// from the user's `0x1500_04E7` to `0`.
    ///
    /// Returns what `disarm_entry_breakpoint` has to undo, or `None` when
    /// nothing was armed — a full unit, or a read that failed. Either way the
    /// explicit halt at the end of `reset_system` still stops the core.
    fn arm_entry_breakpoint(
        &self,
        arm: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
    ) -> Option<ArmedEntryBreakpoint> {
        let (boot_addr_register, boot_addr_name) = if self.secondary_ap.as_ref() == Some(ap) {
            (M4_BOOT_ADDR, "MMU->M4_BOOT_ADDR")
        } else {
            (M7_STORE_ADDR_CUR, "M7_StoreAddr_cur")
        };

        let Ok(boot_addr) = Self::read_word(arm, ap, boot_addr_register) else {
            tracing::debug!(
                "N32H7xx: could not read {boot_addr_name}; skipping the entry breakpoint"
            );
            return None;
        };

        // The vendor reads this address blindly. It is an MMU register on their
        // silicon, but a wrong one would turn the next read into a bus fault, so
        // refuse anything that could not be a vector table before dereferencing.
        if boot_addr == 0 || boot_addr % 4 != 0 {
            tracing::debug!(
                "N32H7xx: {boot_addr_name} reads 0x{boot_addr:08X}, which cannot hold a \
                 vector table; skipping the entry breakpoint"
            );
            return None;
        }

        let Ok(entry) = Self::read_word(arm, ap, u64::from(boot_addr) + 4) else {
            tracing::debug!(
                "N32H7xx: could not read the reset vector; skipping the entry breakpoint"
            );
            return None;
        };

        // On an erased part this reads back as 0xFFFF_FFFF, and masking that
        // would arm the comparator at 0x1FFF_FFFC — an address no real code can
        // match, so the breakpoint would be pure overhead. A real ARMv7-M reset
        // vector is a Thumb address, so bit 0 is set; requiring that rejects the
        // all-zero case too. Either way the explicit halt at the end of
        // `reset_system` is what actually stops a blank part, which locks up
        // before it would ever reach an entry point.
        if entry == u32::MAX || entry & 1 == 0 {
            tracing::debug!(
                "N32H7xx: the reset vector reads 0x{entry:08X}, which does not point at \
                 an application; skipping the entry breakpoint"
            );
            return None;
        }

        // The two cores' FPB units encode a breakpoint differently: the M4's
        // FpRev1 carries the half-word selector in REPLACE (bits 31:30), the
        // M7's FpRev2 the address in bits 31:1. Writing one core's encoding to
        // the other arms a value the other's decoder rejects.
        let comparator_value = if self.secondary_ap.as_ref() == Some(ap) {
            match Self::fp_rev1_comparator_value(entry) {
                Some(value) => value,
                None => {
                    tracing::debug!(
                        "N32H7xx: the entry point 0x{entry:08X} is out of reach of the \
                         FpRev1 comparators; skipping the entry breakpoint"
                    );
                    return None;
                }
            }
        } else {
            (entry & 0x1FFF_FFFC) | 0x01
        };

        // The comparator it goes into: the first one that reads exactly zero,
        // leaving the session's own breakpoints where `set_hw_breakpoint` (from
        // index 0 upwards) put them. Anything non-zero counts as taken — even
        // an undecodable value, which is not ours to judge here. `FP_CTRL` is
        // read along the way because the disarm needs its ENABLE bit.
        let Ok(ctrl) = Self::read_word(arm, ap, FP_CTRL) else {
            tracing::debug!("N32H7xx: could not read FP_CTRL; skipping the entry breakpoint");
            return None;
        };
        let index = match Self::free_fpb_comparator(arm, ap, ctrl) {
            Ok(Some(index)) => index,
            Ok(None) => {
                tracing::debug!(
                    "N32H7xx: every FPB comparator is taken; skipping the entry breakpoint \
                     and leaving the halt to the explicit request"
                );
                return None;
            }
            Err(e) => {
                tracing::debug!(
                    "N32H7xx: could not walk the FPB comparators ({e}); skipping the entry \
                     breakpoint"
                );
                return None;
            }
        };

        let comparator = FP_COMP0 + u64::from(index) * 4;
        if let Err(e) = Self::write_word(arm, ap, comparator, comparator_value) {
            tracing::debug!("N32H7xx: could not set the FPB breakpoint: {e}");
            return None;
        }

        // ENABLE is only accepted alongside KEY, which is the whole point of the
        // odd 0x03 the vendor writes.
        if let Err(e) = Self::write_word(arm, ap, FP_CTRL, 0x03) {
            tracing::debug!("N32H7xx: could not enable the FPB unit: {e}");
            // Do not leave a comparator armed behind a unit that never got
            // enabled: a later walk would read it as a live breakpoint.
            if let Err(e) = Self::write_word(arm, ap, comparator, 0) {
                tracing::debug!("N32H7xx: could not clear the unused comparator: {e}");
            }
            return None;
        }

        tracing::debug!(
            "N32H7xx: FPB breakpoint armed at the application entry point 0x{:08X} \
             (comparator {index})",
            entry & !0b1
        );

        Some(ArmedEntryBreakpoint {
            index,
            was_enabled: ctrl & 1 != 0,
        })
    }

    /// Disarm the entry breakpoint, so the next run is not stopped by it.
    ///
    /// Puts back exactly what arming changed — the one comparator it took, and
    /// `FP_CTRL`'s ENABLE bit as it was found — and nothing else. Zeroing
    /// comparator 0 and turning the unit off unconditionally, as this used to,
    /// deleted the first user breakpoint of the session and left every *other*
    /// one armed but dead until something happened to re-enable the unit
    /// (measured after a `restart`: the M7's `FP_CTRL` read `0x1000_0080`,
    /// disabled, and only the stepping dance at the start of the next `run`
    /// turned it back on).
    fn disarm_entry_breakpoint(
        &self,
        arm: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
        armed: &ArmedEntryBreakpoint,
    ) {
        let comparator = FP_COMP0 + u64::from(armed.index) * 4;
        if let Err(e) = Self::write_word(arm, ap, comparator, 0) {
            tracing::debug!("N32H7xx: could not clear the FPB breakpoint: {e}");
        }
        // KEY, plus ENABLE only if the unit had it: writing 0x02 here would
        // leave breakpoints in the other comparators silently dead.
        if let Err(e) = Self::write_word(arm, ap, FP_CTRL, 0x02 | u32::from(armed.was_enabled)) {
            tracing::debug!("N32H7xx: could not restore the FPB unit's enable bit: {e}");
        }
    }

    /// The `FP_COMPn` value that puts an FpRev1 code breakpoint on `entry`.
    ///
    /// FpRev1 (the M4's unit) encodes the low address bits in the *top* of the
    /// register: `REPLACE = 0b01` selects the word at `0x...0`, `0b10` the
    /// half-word at `0x...2`. Writing the address contiguously from bit 2 — the
    /// way FpRev2 (the M7's) does — leaves `REPLACE = 0b00`, the *remap*
    /// encoding, which the ARM ARM documents as UNPREDICTABLE on a unit without
    /// remap support and which probe-rs's own decoder rejects outright.
    /// `None` for an address the comparators cannot cover: FpRev1 compares only
    /// the code region, the same `0x2000_0000` ceiling probe-rs's own
    /// `FpRev1CompX::breakpoint_configuration` enforces.
    fn fp_rev1_comparator_value(entry: u32) -> Option<u32> {
        // Bit 0 of a vector is the Thumb marker, not part of the address.
        let address = entry & !0b1;
        if address >= 0x2000_0000 {
            return None;
        }

        let replace = if address & 0b11 == 0 { 0b01 } else { 0b10 };
        Some((replace << 30) | (address & 0x1FFF_FFFC) | 0x01)
    }

    /// How many code comparators this FPB has, read out of `FP_CTRL`.
    ///
    /// The same fields probe-rs's `FpCtrl::num_code()` reads; revision 1 splits
    /// the count across two bit ranges. `None` for a revision whose layout this
    /// sequence does not know, which callers treat as "do not touch the unit".
    fn fpb_num_code(ctrl: u32) -> Option<u32> {
        match ctrl >> 28 {
            0 => Some((ctrl >> 4) & 0xF),
            1 => Some((((ctrl >> 12) & 0x7) << 4) | ((ctrl >> 4) & 0xF)),
            _ => None,
        }
    }

    /// The first comparator that reads exactly zero, given the unit's `FP_CTRL`.
    ///
    /// "Free" means exactly zero and nothing else. A comparator holding a value
    /// that does not decode is still holding it, and that junk is
    /// `sanitize_secondary_fpb`'s to deal with at the moments a session can
    /// afford it — overwriting it here would just hide what the next
    /// normalization is looking for. `None` covers both a full unit and one
    /// whose layout cannot be walked, which callers treat the same way: no
    /// entry breakpoint, and the explicit halt has to stop the core.
    fn free_fpb_comparator<D: DapAccess + ?Sized>(
        iface: &mut D,
        ap: &FullyQualifiedApAddress,
        ctrl: u32,
    ) -> Result<Option<u32>, ArmError> {
        let Some(num_code) = Self::fpb_num_code(ctrl) else {
            tracing::debug!(
                "N32H7xx: the FPB reports revision {}, which this sequence does not know \
                 how to walk",
                ctrl >> 28
            );
            return Ok(None);
        };

        for index in 0..num_code {
            if Self::read_word(iface, ap, FP_COMP0 + u64::from(index) * 4)? == 0 {
                return Ok(Some(index));
            }
        }

        Ok(None)
    }

    /// Put the second core's FPB back in a state `hw_breakpoints()` can walk.
    ///
    /// The M4's FPB is part of the domain `RCC->M4RSTREL` gates, and its
    /// comparators' reset values are documented as unknown; measured, an
    /// N32H785 was left with `FP_COMP2 = 0x0000_0001` by a session that had
    /// cycled the M4. That value does not decode as a breakpoint, and
    /// `hw_breakpoints()` turns a decode failure into an error that aborts the
    /// whole walk — so `clear_all_hw_breakpoints` fails, and `Session::new`
    /// propagates that straight out of the attach (session.rs:212). The board
    /// is then unfixable through the debugger: the only code that would clear
    /// the offending value is the very clear that fails.
    ///
    /// So clear comparators that hold anything *before* the session's clear
    /// walks them. Writing zeros is exactly what probe-rs's clear does for
    /// every breakpoint it recognises; skipping the decoding step is what makes
    /// this work on values it does not recognise. `FP_CTRL` is left as found.
    ///
    /// Best-effort: a failure here must not cost the session — everything this
    /// repairs is also a state the session could have been in before this
    /// existed — so callers log it and move on.
    fn sanitize_secondary_fpb<D: DapAccess + ?Sized>(
        iface: &mut D,
        ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let ctrl = Self::read_word(iface, ap, FP_CTRL)?;

        // An unknown revision means an unknown layout, so there is nothing to
        // walk and nothing to safely clear.
        let Some(num_code) = Self::fpb_num_code(ctrl) else {
            tracing::debug!(
                "N32H7xx: the second core's FPB reports revision {}, which this sequence \
                 does not know how to walk; leaving it alone",
                ctrl >> 28
            );
            return Ok(());
        };

        for index in 0..num_code {
            let address = FP_COMP0 + u64::from(index) * 4;
            let value = Self::read_word(iface, ap, address)?;
            if value == 0 {
                continue;
            }

            tracing::info!(
                "N32H7xx: the second core's FPB comparator {index} held 0x{value:08X}; \
                 clearing it"
            );
            Self::write_word(iface, ap, address, 0)?;
        }

        Ok(())
    }

    /// Capture the second core's FPB, so `restore_secondary_fpb` can put it
    /// back once that core has been reset.
    ///
    /// The unit sits in the domain `RCC->M4RSTREL` gates, so resetting the core
    /// leaves the comparators holding fresh hardware state — measured, all zero
    /// — and every breakpoint the session had armed on it is gone. For the
    /// debugger's `restart` that is the whole bug: the client set breakpoints,
    /// they were verified, and after the restart they silently never fire again.
    /// Capturing the values before the reset is what makes putting them back
    /// afterwards possible.
    ///
    /// `Ok(None)` for a layout this sequence cannot walk, and an `Err` for a
    /// read that failed; either way the caller falls back to the clearing
    /// normalization.
    fn snapshot_secondary_fpb<D: DapAccess + ?Sized>(
        iface: &mut D,
        ap: &FullyQualifiedApAddress,
    ) -> Result<Option<FpbSnapshot>, ArmError> {
        let ctrl = Self::read_word(iface, ap, FP_CTRL)?;

        let Some(num_code) = Self::fpb_num_code(ctrl) else {
            tracing::debug!(
                "N32H7xx: the second core's FPB reports revision {}, which this sequence \
                 does not know how to walk; its breakpoints cannot be captured",
                ctrl >> 28
            );
            return Ok(None);
        };

        let mut comparators = Vec::with_capacity(num_code as usize);
        for index in 0..num_code {
            comparators.push(Self::read_word(iface, ap, FP_COMP0 + u64::from(index) * 4)?);
        }

        tracing::debug!(
            "N32H7xx: captured the second core's FPB before its reset: \
             FP_CTRL=0x{ctrl:08X}, comparators={comparators:X?}"
        );

        Ok(Some(FpbSnapshot {
            enabled: ctrl & 1 != 0,
            comparators,
        }))
    }

    /// Put the second core's FPB back the way `snapshot_secondary_fpb` found it.
    ///
    /// This is what lets a session keep its breakpoints across a reset of the
    /// core they are armed on. The entry breakpoint, when one was armed, is
    /// removed as a side effect: it went into a comparator the snapshot recorded
    /// as zero, so writing the snapshot back clears it — and the later
    /// `disarm_entry_breakpoint` then writes the same values again.
    ///
    /// `FP_CTRL` goes last: the unit may only match on a complete set of
    /// comparators, or a half-restored one could stop the core at an address
    /// nobody asked about.
    fn restore_secondary_fpb<D: DapAccess + ?Sized>(
        iface: &mut D,
        ap: &FullyQualifiedApAddress,
        snapshot: &FpbSnapshot,
    ) -> Result<(), ArmError> {
        for (index, value) in snapshot.comparators.iter().enumerate() {
            Self::write_word(iface, ap, FP_COMP0 + index as u64 * 4, *value)?;
        }

        // KEY, plus ENABLE as the snapshot found it.
        Self::write_word(iface, ap, FP_CTRL, 0x02 | u32::from(snapshot.enabled))?;

        tracing::debug!("N32H7xx: put the second core's FPB back after its reset");
        Ok(())
    }

    /// Normalize the second core's FPB — but only at the moments a session can
    /// afford it.
    ///
    /// `on_attach` runs on *every* `Session::core()` call: the attachment is not
    /// cached, so every request that touches the core re-enters it. Normalizing
    /// unconditionally therefore clears, on the next request's attach, the
    /// breakpoints the previous request just armed — `setBreakpoints` reports
    /// `verified=true`, and the core then never stops for any of them, because
    /// the `continue` that should have hit one wiped it on the way in. Two
    /// attaches are the right ones, and they both announce themselves:
    ///
    /// * The *first* attach of a session. It happens while `Session::new` is
    ///   still building the session (`clear_all_hw_breakpoints` attaches to
    ///   every core), so nothing can have armed a breakpoint yet — and it comes
    ///   before that same clear walks the comparators, which is where a value
    ///   left behind by an earlier run would otherwise fail the walk and take
    ///   the whole attach with it.
    /// * An attach that just released the core, `released_now`. The release is
    ///   precisely what makes whatever the comparators hold fresh hardware state
    ///   rather than anyone's breakpoint — measured, a core that has been cycled
    ///   can come back with `FP_COMP2 = 0x0000_0001`, which `hw_breakpoints()`
    ///   cannot decode and which would fail the very `set_hw_breakpoint` that
    ///   comes next.
    ///
    /// The latch lives on this sequence, and one sequence is built per session,
    /// so a session that ends with the FPB still poisoned is healed by the next
    /// one. (`Relaxed` is enough: attaches to one session are serialized by
    /// `&mut Session`, so the latch only ever skips work that some earlier
    /// attach in the same session has already done.)
    fn normalize_secondary_fpb_if_due<D: DapAccess + ?Sized>(
        &self,
        iface: &mut D,
        ap: &FullyQualifiedApAddress,
        released_now: bool,
    ) {
        let first_attach = !self.secondary_fpb_sanitized.swap(true, Ordering::Relaxed);
        if !first_attach && !released_now {
            return;
        }

        if let Err(e) = Self::sanitize_secondary_fpb(iface, ap) {
            tracing::warn!(
                "N32H7xx: could not normalize the second core's FPB ({e}); a value \
                 left behind by an earlier run may still block this session"
            );
            Self::clear_sticky_errors(iface, ap.dp());
        }
    }

    /// Ask for a reset via AIRCR, tolerating the failure it causes.
    ///
    /// The write failing is the normal outcome, not an error: the reset it
    /// requests takes the AP away mid-transaction. The vendor suppresses exactly
    /// this error in their own `ResetSystem` sequence.
    fn request_reset(
        &self,
        arm: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
        value: u32,
    ) {
        if let Err(e) = Self::write_word(arm, ap, Aircr::get_mmio_address(), value) {
            tracing::warn!(
                "N32H7xx: AIRCR write reported {e}; expected, the reset it requests \
                 takes the AP away before the write can complete"
            );
        }
    }
}

impl ArmDebugSequence for N32H7xx {
    /// Bring up the second core, but only when someone actually asks for it.
    ///
    /// `Session::new` walks every core in the chip description and fails the whole
    /// session if one of them cannot be reached, so a dual-core description needs
    /// the M4's AP to be up before the session can be built *and* the M4 has to
    /// stay in reset until then — those two are in direct conflict, and this is
    /// resolved in `debug_core_start`, which is the hook that walk actually uses.
    /// `on_attach` is called only for the core that is being used, which makes it
    /// the first place where releasing the M4 is both safe and wanted.
    ///
    /// A no-op for the primary core, and therefore also for the single-core
    /// 73x/76x parts.
    fn on_attach(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        core_type: CoreType,
        target: &mut crate::Target,
    ) -> Result<(), ArmError> {
        if self.secondary_ap.as_ref() == Some(core_ap) {
            let released_now = match self.release_secondary_core(interface) {
                Ok(released_now) => released_now,
                Err(e) => {
                    // A failed AP access leaves STICKYERR set, which would block
                    // every later access through this DP as well.
                    tracing::warn!("N32H7xx: the second core could not be released: {e}");
                    self.clear_dp_errors(interface, core_ap);
                    return Err(ArmError::CoreDisabled);
                }
            };

            if released_now {
                // We are the ones who let it out, so it is sitting on the first
                // instruction of whatever `MMU->M4_BOOT_ADDR` points at. Stop it
                // there rather than letting it run: on a part whose Bank2 has
                // never been programmed, that instruction is 0xFFFFFFFF, and the
                // M4 taking a fault is what takes the M7 down with it.
                if let Err(e) = self.halt_and_confirm(interface, core_ap) {
                    tracing::warn!(
                        "N32H7xx: the second core was released but would not halt ({e}); \
                         leaving it out of this session"
                    );
                    return Err(ArmError::CoreDisabled);
                }
            }

            if !self.ap_is_alive(interface, core_ap) {
                tracing::warn!(
                    "N32H7xx: the second core's AP does not answer after the release; \
                     leaving it out of this session"
                );
                return Err(ArmError::CoreDisabled);
            }

            // Only at the moments a session can afford it. This has to run on
            // the first attach — before `clear_all_hw_breakpoints` walks the
            // comparators, where an undecodable value would fail the whole
            // attach — and it must *not* run on the attaches in between, which
            // would clear this session's own breakpoints. See the function's
            // doc.
            self.normalize_secondary_fpb_if_due(interface, core_ap, released_now);
        }

        DefaultArmSequence(()).on_attach(interface, core_ap, core_type, target)
    }

    /// Keep `Session::new`'s walk off the second core's AP until `on_attach` brings
    /// it up.
    ///
    /// The walk calls this for every core the chip description declares and gives
    /// up on the entire session if any one of them fails. The M4 is held in reset
    /// at that point — by design, and necessarily, since releasing it before the
    /// session exists is exactly what breaks M7 flashing — so its AP cannot
    /// answer, and asking it to would fail the session. Reporting success here
    /// costs nothing: nothing has been attached, and `on_attach` does the real
    /// work on the way in.
    fn debug_core_start(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        core_type: CoreType,
        debug_base: Option<u64>,
        cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        if self.secondary_ap.as_ref() == Some(core_ap) {
            tracing::debug!(
                "N32H7xx: skipping debug setup for the second core; it is still in reset \
                 and is brought up if and when it is used"
            );
            return Ok(());
        }

        DefaultArmSequence(()).debug_core_start(interface, core_ap, core_type, debug_base, cti_base)
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let ap = interface.fully_qualified_address();

        // `reset_and_halt` arms DEMCR.VC_CORERESET before calling us, `Core::reset`
        // does not. That bit is how we tell the two callers apart, and therefore
        // whether the core is expected to be halted when we return.
        let demcr_raw = interface.read_word_32(Demcr::get_mmio_address()).ok();
        let halt_after = demcr_raw
            .map(|value| Demcr(value).vc_corereset())
            .unwrap_or(false);

        // SEGGER's script is explicit — "Don't halt core after reset via
        // DEMCR.VC_CORERESET" — and this is measured, not inherited: with the bit
        // armed, writing SYSRESETREQ leaves the AHB-AP permanently dead (the DP
        // still answers, the AP never does, and only a power cycle recovers it).
        // The vector catch fires the moment the core leaves reset, which is inside
        // the boot window — and this silicon has SWD disabled during boot, so
        // stopping the core there stops boot from ever un-gating it. `probe-rs
        // reset` never arms the bit, which is why that command survives the very
        // same write.
        //
        // Clear it before touching AIRCR. The halt the caller is waiting for comes
        // from `halt_and_confirm` at the end of this function instead, so nothing
        // depends on vector catch firing.
        if let Some(raw) = demcr_raw {
            let mut demcr = Demcr(raw);
            if demcr.vc_corereset() {
                demcr.set_vc_corereset(false);
                match interface.write_word_32(Demcr::get_mmio_address(), demcr.into()) {
                    Ok(()) => {
                        tracing::debug!("N32H7xx: cleared DEMCR.VC_CORERESET before the reset")
                    }
                    Err(e) => tracing::warn!(
                        "N32H7xx: could not clear DEMCR.VC_CORERESET ({e}); this reset will \
                         likely leave the AHB-AP dead"
                    ),
                }
            }
        }

        tracing::info!("N32H7xx: running reset sequence (halt_after={halt_after})");

        let arm = interface.get_arm_debug_interface()?;

        // A reset of the second core takes its FPB down with it — the unit sits
        // in the domain `RCC->M4RSTREL` gates, and measured, the comparators
        // come back holding nothing but zeros. What they hold *now* is the
        // session's own breakpoints, and only until the reset: capture them
        // first, so they can be put back once the core is running again. Taken
        // before the entry breakpoint is armed, both so the snapshot holds the
        // user's breakpoints and so restoring it removes the entry breakpoint
        // again as a side effect.
        let fpb_snapshot = if self.secondary_ap.as_ref() == Some(&ap) {
            match Self::snapshot_secondary_fpb(arm, &ap) {
                Ok(snapshot) => snapshot,
                Err(e) => {
                    tracing::warn!(
                        "N32H7xx: could not capture the second core's FPB before its \
                         reset ({e}); breakpoints armed on it will not survive"
                    );
                    self.clear_dp_errors(arm, &ap);
                    None
                }
            }
        } else {
            None
        };

        // Only when the caller wants a halted core: for `Core::reset` an armed
        // breakpoint on the reset vector would stop a core that is supposed to be
        // left running.
        //
        // The second core is left out on purpose: its FPB goes down with the
        // gated domain the reset takes away, so a comparator armed here would
        // be gone before the core could ever run into it. The secondary branch
        // arms its own afterwards, on the unit that has come back with the core.
        let mut entry_breakpoint = None;
        if halt_after && self.secondary_ap.as_ref() != Some(&ap) {
            entry_breakpoint = self.arm_entry_breakpoint(arm, &ap);
        }

        // SEGGER calls this immediately before writing AIRCR, and that is where
        // it belongs: arming the breakpoint above reads a vendor MMU register,
        // and a fault there would otherwise still be sticky when AIRCR goes out.
        self.clear_dp_errors(arm, &ap);

        // SYSRESETREQ, exactly as the vendor's `ResetSystem` does it. The vendor
        // also offers a core-local `VECTRESET` reset (`ResetProcessor`), but that
        // leaves peripherals, clocks and the flash controller untouched, so it is
        // a strictly weaker reset and is not used here.
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        self.request_reset(arm, &ap, aircr.into());

        // Do not touch the AP while the reset is still running.
        thread::sleep(RESET_SETTLE);

        // Which AP the recovery loop watches. On a reset of the *second* core it
        // is the primary core's, not the one being reset: the whole-SoC reset
        // gates the M4 again, so the M4's AP cannot answer until the primary core
        // is back and the release below has been redone. Polling the gated AP
        // instead — which is what this did — is not a poll that fails: measured
        // on a probe that does not retry a refused transfer internally (ULINK2),
        // the `TAR` write goes out, the `DRW` read is never answered at all, and
        // the probe waits out its full USB timeout on it while the DP operations
        // queued behind it fail too — the release that would have brought the AP
        // back among them. The primary AP is what drives that release, so it is
        // the one worth waiting for; `rerelease_secondary_core` then waits for the
        // second AP itself, through the raw IDR read a gated AP refuses instead of
        // stalling on.
        let resetting_secondary = self.secondary_ap.as_ref() == Some(&ap);
        let watch = if resetting_secondary {
            self.primary_ap.clone()
        } else {
            ap.clone()
        };

        let deadline = Instant::now() + RECOVERY_TIMEOUT;
        let mut attempts = 0u32;
        let mut alive = self.ap_is_alive(arm, &watch);

        while !alive && Instant::now() < deadline {
            attempts += 1;
            tracing::debug!("N32H7xx: AP silent after reset, recovery attempt {attempts}");

            self.clear_dp_errors(arm, &watch);

            // Tears the debug port down and brings it back up from scratch, which
            // re-issues the power-up requests the reset may have dropped.
            if let Err(e) = arm.reinitialize() {
                tracing::debug!("N32H7xx: reinitialize failed: {e}");
            }

            thread::sleep(Duration::from_millis(25));
            alive = self.ap_is_alive(arm, &watch);
        }

        if alive {
            tracing::debug!("N32H7xx: AP back after {attempts} recovery attempt(s)");
        } else {
            tracing::warn!(
                "N32H7xx: the AHB-AP at {watch:?} never came back after the reset; \
                 continuing without one, which the flash procedure does not need"
            );
        }

        // A whole-SoC reset gates the second core again: `M4RSTREL` goes back to its
        // reset value of 0, which takes the M4's AP with it. Putting it back is only
        // the right thing to do when the core being reset *is* the second core —
        // releasing it because the M7 was reset would be the very thing that breaks
        // M7 flashing, and it would also overrule firmware that parked the M4 on
        // purpose.
        //
        // The release therefore has to happen before the *first* access through that
        // AP, and what it reports decides whether the block below may run at all: on
        // a gated AP the accesses it needs — a halt, a walk over the FPB — are the
        // ones measured above to hang a probe without internal retries, and the
        // breakpoints it would put back belong to a session that is about to fail on
        // that same AP anyway.
        let secondary_usable = resetting_secondary && self.rerelease_secondary_core(arm);

        if secondary_usable {
            // Freshly re-released, the core sits on the first instruction of
            // whatever its boot address points at — on a part whose Bank2 has
            // never been programmed, 0xFFFFFFFF, and the fault that follows is
            // what takes the M7 down with it (see the module docs). Stop it
            // before anything slow happens: the M7's halt further down can take
            // hundreds of milliseconds, and it is not the only core that wants
            // stopping.
            if halt_after {
                match self.halt_and_confirm(arm, &ap) {
                    Ok(()) => {
                        tracing::debug!("N32H7xx: halted the second core right after its release")
                    }
                    Err(e) => {
                        tracing::warn!(
                            "N32H7xx: the re-released second core would not halt ({e}); \
                             the halt at the end of this reset will ask again"
                        );
                        self.clear_dp_errors(arm, &ap);
                    }
                }
            }

            // The reset left the comparators holding fresh hardware state: the
            // session's breakpoints are gone from the unit. Put back what the
            // snapshot captured — for a debugger `restart` this is the whole
            // difference between a session that restarts and one whose
            // breakpoints silently never fire again.
            match fpb_snapshot {
                Some(snapshot) => {
                    if let Err(e) = Self::restore_secondary_fpb(arm, &ap, &snapshot) {
                        tracing::warn!(
                            "N32H7xx: could not put the second core's FPB back after its \
                             reset ({e}); breakpoints armed on it are lost"
                        );
                        self.clear_dp_errors(arm, &ap);
                    }
                }
                // No snapshot — a layout this sequence cannot walk, or a read
                // that failed. Fall back to the clearing normalization, which
                // at least keeps the next walk over the comparators from
                // choking on an undecodable value.
                None => {
                    if let Err(e) = Self::sanitize_secondary_fpb(arm, &ap) {
                        tracing::debug!("N32H7xx: could not normalize the second core's FPB: {e}");
                        self.clear_dp_errors(arm, &ap);
                    }
                }
            }

            // The entry breakpoint the M4 never got before the reset — it is
            // armed here, on the unit that has come back with the core, and the
            // core is then sent through its own reset for the breakpoint to have
            // anything to catch. Arming alone would not do: the core is halted
            // (the halt right after the release above), and a comparator is only
            // ever matched by executing past it. Letting it go instead of
            // resetting it is what does not work — measured, the released core
            // reaches its main loop before a halt request can land — and taking
            // it through `RCC->M4RSTREL` again would take the FPB down with it.
            // `VECTRESET` is the vendor's own `ResetProcessor` shape: the core
            // restarts from its vector table, the debug domain stays up, and the
            // armed comparator stops it on the application's first instruction.
            if halt_after {
                match self.arm_entry_breakpoint(arm, &ap) {
                    Some(armed) => match self.vectreset(arm, &ap) {
                        Ok(()) => {
                            tracing::debug!(
                                "N32H7xx: sent the second core through VECTRESET; its entry \
                                 breakpoint will stop it at its first instruction"
                            );
                            entry_breakpoint = Some(armed);
                        }
                        Err(e) => {
                            tracing::warn!(
                                "N32H7xx: could not send the second core through its own \
                                 reset ({e}); it stays halted where the release put it"
                            );
                            self.disarm_entry_breakpoint(arm, &ap, &armed);
                            self.clear_dp_errors(arm, &ap);
                        }
                    },
                    // No comparator to catch it with — a full unit, or a vector
                    // table this sequence cannot read. Restarting it anyway
                    // would only lose the halt; it stays where the release put
                    // it, which is what this did before the entry breakpoint
                    // existed.
                    None => tracing::debug!(
                        "N32H7xx: no entry breakpoint could be armed on the second core; \
                         leaving it halted where the release put it"
                    ),
                }
            }

            // The vendor's M4 reset path also stops the M7 and keeps it stopped
            // (`releaseM4AndHaltM7` runs for every download): their comment is
            // that a still-running M7 — its DMA, its peripherals — is what makes
            // M4 downloads fail. The whole-SoC reset above restarts the M7 too,
            // so whatever it does next lands inside the second core's work; a
            // halt request before the caller gets to the M4 is that same
            // suppression.
            //
            // Gated on `halt_after`: that is the download path and the debugger's
            // reset-and-halt, where a stopped chip is the point. `Core::reset`
            // promises a running core and must not stop the other one either.
            if halt_after {
                match self.halt_and_confirm(arm, &self.primary_ap) {
                    Ok(()) => tracing::info!(
                        "N32H7xx: halted the primary core for the second core's reset"
                    ),
                    Err(e) => {
                        // A failed halt may have left sticky errors behind on a
                        // DP the caller still needs; clear them and carry on.
                        tracing::warn!(
                            "N32H7xx: could not halt the primary core for the second \
                             core's reset ({e})"
                        );
                        self.clear_dp_errors(arm, &self.primary_ap);
                    }
                }
            }
        }

        // Re-enable debug on the core: the reset cleared it.
        if let Err(e) = self.debug_core_start(arm, &ap, core_type, debug_base, None) {
            tracing::debug!("N32H7xx: debug_core_start failed: {e}");
        }

        // `reset_and_halt` wants a halted core when we return. The entry
        // breakpoint may already have provided one; if it has not — an
        // unprogrammed flash never reaches the entry point, it locks up instead —
        // or if the reset never ran, do it explicitly. `Core::reset` leaves the
        // core running, so only halt when the caller asked for it.
        //
        // On the second core's path that halt is only worth asking for once its AP
        // has answered: a halt request through a gated AP is the access measured to
        // hang a probe that does not retry internally, and the caller's own
        // `wait_for_core_halted` reports the same failure either way.
        let core_usable = !resetting_secondary || secondary_usable;
        if core_usable
            && (halt_after || !alive)
            && let Err(e) = self.halt_and_confirm(arm, &ap)
        {
            tracing::warn!(
                "N32H7xx: the core would not halt after the reset ({e}); the caller \
                 will time out waiting for it"
            );
        }

        // The breakpoint has done its job now the core is stopped; leaving it
        // armed would stop the next run at the same address.
        if let Some(armed) = &entry_breakpoint {
            self.disarm_entry_breakpoint(arm, &ap, armed);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // Two different `ApAddress` types are in play: the chip description carries
    // `probe_rs_target`'s, while a resolved `FullyQualifiedApAddress` reports the
    // architecture layer's. Aliased so neither one is mistaken for the other.
    use crate::architecture::arm::ApAddress as ArmApAddress;
    use crate::architecture::arm::armv7m::FpRev1CompX;
    use crate::architecture::arm::dp::DpRegisterAddress;
    use probe_rs_target::{
        ApAddress as TargetApAddress, ArmCoreAccessOptions, Core, CoreAccessOptions,
    };

    // Chip descriptions are built by hand here rather than loaded from a
    // generated YAML, so these pin the *logic* — which core counts as primary,
    // when a second one exists, and which CPUID each reports — without depending
    // on the vendor's pack files.
    //
    // They carry more weight than tests usually do. The dual-core paths have been
    // exercised on an N32H785, but the single-core 73x/76x parts have never been
    // run on hardware at all, which leaves
    // `single_core_parts_have_no_second_core_to_release` as the only thing
    // standing behind the claim that they behave as before.

    /// A chip description with one ARM core on the given AP.
    fn single_core(name: &str, ap: u8) -> Chip {
        let mut chip = Chip::generic_arm(name, CoreType::Armv7em);
        chip.cores[0].core_access_options = CoreAccessOptions::Arm(ArmCoreAccessOptions {
            ap: TargetApAddress::V1(ap),
            ..Default::default()
        });
        chip
    }

    /// The vendor's dual-core layout: M7 on AP0, M4 on AP2.
    fn dual_core(name: &str) -> Chip {
        let mut chip = single_core(name, 0);
        chip.cores.push(Core {
            name: "cm4".to_string(),
            core_type: CoreType::Armv7em,
            core_access_options: CoreAccessOptions::Arm(ArmCoreAccessOptions {
                ap: TargetApAddress::V1(2),
                ..Default::default()
            }),
        });
        chip
    }

    #[test]
    fn single_core_parts_have_no_second_core_to_release() {
        // The 73x/76x parts are what keeps this sequence's old behaviour intact:
        // with no second AP every dual-core path is a no-op.
        for name in ["N32H765xIx7", "N32H730xIx7", "N32H762xKx7"] {
            let sequence = N32H7xx::create_for_chip(&single_core(name, 0));
            assert!(
                sequence.secondary_ap.is_none(),
                "{name} must not have a second core"
            );
            assert_eq!(
                sequence.expected_cpuid(&sequence.primary_ap),
                Some(EXPECTED_CPUID)
            );
        }
    }

    #[test]
    fn dual_core_parts_pick_the_second_ap_out_of_the_core_list() {
        for name in ["N32H785xIx7", "N32H787xKx7"] {
            let sequence = N32H7xx::create_for_chip(&dual_core(name));

            let secondary = sequence
                .secondary_ap
                .as_ref()
                .unwrap_or_else(|| panic!("{name} must have a second core"));

            assert_eq!(secondary.ap(), &ArmApAddress::V1(2));
            assert_ne!(secondary, &sequence.primary_ap);

            // Each core is checked against its own part number — the whole point
            // of the per-AP CPUID, since a single constant would call every
            // healthy M4 a dead AP.
            assert_eq!(sequence.expected_cpuid(secondary), Some(EXPECTED_CPUID_M4));
            assert_eq!(
                sequence.expected_cpuid(&sequence.primary_ap),
                Some(EXPECTED_CPUID)
            );
        }
    }

    #[test]
    fn cores_are_told_apart_by_ap_not_by_name() {
        // vendor descriptions have used both `cm4` and `CM4`; what actually pins
        // the core down is the AP number it is reachable on.
        let mut chip = dual_core("N32H785xIx7");
        chip.cores[1].name = "the second one".to_string();

        let sequence = N32H7xx::create_for_chip(&chip);
        assert_eq!(
            sequence
                .secondary_ap
                .as_ref()
                .map(FullyQualifiedApAddress::ap),
            Some(&ArmApAddress::V1(2))
        );
    }

    #[test]
    fn fp_rev1_entry_breakpoints_agree_with_probe_rs_own_encoder() {
        // The M4's comparator value is what a later `hw_breakpoints()` walk has
        // to decode; pinning it against probe-rs's own encoder makes "the armed
        // breakpoint is decodable" a property of this code rather than of the
        // silicon it happened to be tried on.
        for entry in [0x1500_0401u32, 0x1500_0403, 0x1500_0405, 0x1508_0001] {
            let address = entry & !0b1;
            assert_eq!(
                N32H7xx::fp_rev1_comparator_value(entry),
                Some(u32::from(
                    FpRev1CompX::breakpoint_configuration(address)
                        .expect("a code-region address is encodable")
                )),
                "entry 0x{entry:08X}"
            );
        }
    }

    #[test]
    fn fp_rev1_entry_breakpoints_refuse_addresses_out_of_reach() {
        // FpRev1 comparators only cover the code region. The ceiling is
        // probe-rs's own — the encoder refuses the same addresses — and this
        // sequence must refuse them before arming anything.
        assert_eq!(N32H7xx::fp_rev1_comparator_value(0x2000_0001), None);
        assert_eq!(N32H7xx::fp_rev1_comparator_value(0x2400_0001), None);
        assert!(
            FpRev1CompX::breakpoint_configuration(0x2000_0000).is_err(),
            "probe-rs's own encoder must refuse the same ceiling"
        );
    }

    /// The sliver of an AP the FPB normalization talks to: `CSW`, `TAR` and
    /// `DRW` raw register access, with `DRW` resolving through the stored `TAR`
    /// into a flat address space the way the real AP does. That is the whole of
    /// the raw word path `sanitize_secondary_fpb` uses.
    struct FpbMock {
        csw: u32,
        tar: u32,
        memory: HashMap<u64, u32>,
    }

    impl FpbMock {
        /// An FPB that reads like the M4's — `FP_CTRL = 0x261`, the value
        /// measured on an N32H785 (revision 0, six comparators) — with the
        /// given comparators seeded. Everything else reads as zero.
        fn with_measured_m4_fpb(comparators: &[(u32, u32)]) -> Self {
            let mut memory = HashMap::new();
            memory.insert(FP_CTRL, 0x261);
            for (index, value) in comparators {
                memory.insert(FP_COMP0 + u64::from(*index) * 4, *value);
            }
            Self {
                // Word-sized, the way a session leaves an AP in practice.
                csw: CSW_WORD_ACCESS,
                tar: 0,
                memory,
            }
        }

        fn comparator(&self, index: u32) -> u32 {
            self.memory
                .get(&(FP_COMP0 + u64::from(index) * 4))
                .copied()
                .unwrap_or(0)
        }
    }

    impl DapAccess for FpbMock {
        fn read_raw_dp_register(
            &mut self,
            _dp: DpAddress,
            _addr: DpRegisterAddress,
        ) -> Result<u32, ArmError> {
            Ok(0)
        }

        fn write_raw_dp_register(
            &mut self,
            _dp: DpAddress,
            _addr: DpRegisterAddress,
            _value: u32,
        ) -> Result<(), ArmError> {
            Ok(())
        }

        fn read_raw_ap_register(
            &mut self,
            _ap: &FullyQualifiedApAddress,
            addr: u64,
        ) -> Result<u32, ArmError> {
            match addr {
                CSW::ADDRESS => Ok(self.csw),
                TAR::ADDRESS => Ok(self.tar),
                DRW::ADDRESS => Ok(self.memory.get(&u64::from(self.tar)).copied().unwrap_or(0)),
                _ => Ok(0),
            }
        }

        fn write_raw_ap_register(
            &mut self,
            _ap: &FullyQualifiedApAddress,
            addr: u64,
            value: u32,
        ) -> Result<(), ArmError> {
            match addr {
                CSW::ADDRESS => self.csw = value,
                TAR::ADDRESS => self.tar = value,
                DRW::ADDRESS => {
                    let address = u64::from(self.tar);
                    // `FP_CTRL` is mostly read-only configuration, and a
                    // faithful mock has to keep it that way: the revision and
                    // comparator count do not change across a write (measured —
                    // the unit still reads back its count after being written
                    // 0x03), and KEY reads as 0 because it is write-only. Only
                    // ENABLE is writable state.
                    let value = if address == FP_CTRL {
                        (value & 1) | (self.memory.get(&address).copied().unwrap_or(0) & !1)
                    } else {
                        value
                    };
                    self.memory.insert(address, value);
                }
                _ => {}
            }
            Ok(())
        }
    }

    #[test]
    fn later_attaches_leave_the_second_cores_breakpoints_alone() {
        // The regression this pins: `on_attach` runs on every `Session::core()`
        // call, so an unconditional normalization clears, on the next request's
        // attach, the breakpoint the previous request just armed. Measured on an
        // N32H785: `setBreakpoints` read back `verified=true`, and the core then
        // never stopped — the comparator was gone before the `continue` ran.
        let sequence = N32H7xx::create_for_chip(&dual_core("N32H785xIx7"));
        let secondary = sequence
            .secondary_ap
            .clone()
            .expect("the dual-core description has a second AP");

        // A unit with the measured poison in it: a comparator holding a value
        // `hw_breakpoints()` cannot decode.
        let mut fpb = FpbMock::with_measured_m4_fpb(&[(2, 0x0000_0001)]);

        // The session's first attach — the one `Session::new` reaches while
        // `clear_all_hw_breakpoints` walks the cores — heals it.
        sequence.normalize_secondary_fpb_if_due(&mut fpb, &secondary, false);
        assert_eq!(fpb.comparator(2), 0, "the poison must be cleared");
        assert_eq!(
            fpb.memory.get(&FP_CTRL),
            Some(&0x261),
            "FP_CTRL must be left as found"
        );

        // A breakpoint armed afterwards, the way `setBreakpoints` leaves it...
        fpb.write_raw_ap_register(&secondary, TAR::ADDRESS, FP_COMP0 as u32)
            .unwrap();
        fpb.write_raw_ap_register(&secondary, DRW::ADDRESS, 0x5508_04D9)
            .unwrap();

        // ...must survive every later attach of the same session.
        for _ in 0..3 {
            sequence.normalize_secondary_fpb_if_due(&mut fpb, &secondary, false);
        }
        assert_eq!(
            fpb.comparator(0),
            0x5508_04D9,
            "a breakpoint of this session's own must survive the attaches that follow"
        );
    }

    #[test]
    fn an_attach_that_released_the_core_normalizes_again() {
        // A release is what makes the comparators fresh hardware state rather
        // than a breakpoint — and the measured junk a cycled core can come back
        // with (`FP_COMP2 = 0x0000_0001`) is exactly what the session's own
        // `setBreakpoints` cannot cope with. The latch must not stop this call,
        // even though the first attach has long since passed.
        let sequence = N32H7xx::create_for_chip(&dual_core("N32H785xIx7"));
        let secondary = sequence.secondary_ap.clone().unwrap();
        let mut fpb = FpbMock::with_measured_m4_fpb(&[]);

        sequence.normalize_secondary_fpb_if_due(&mut fpb, &secondary, false);
        fpb.write_raw_ap_register(&secondary, TAR::ADDRESS, FP_COMP0 as u32)
            .unwrap();
        fpb.write_raw_ap_register(&secondary, DRW::ADDRESS, 0x5508_04D9)
            .unwrap();

        // A mid-session reset gated the core again; the attach that releases it
        // must not leave the fresh junk in place for `setBreakpoints` to trip on.
        fpb.memory.insert(FP_COMP0 + 8, 0x0000_0001);
        sequence.normalize_secondary_fpb_if_due(&mut fpb, &secondary, true);
        assert_eq!(
            fpb.comparator(2),
            0,
            "the junk the release exposed must be cleared"
        );
        assert_eq!(
            fpb.comparator(0),
            0,
            "whatever else the comparators hold is fresh state too, not a breakpoint"
        );
    }

    #[test]
    fn a_new_session_normalizes_the_fpb_again() {
        // The latch lives on the sequence, and one sequence is built per
        // session. Were it a shared constant instead, a session opened after a
        // crash could never heal what the crashed one left behind.
        let first = N32H7xx::create_for_chip(&dual_core("N32H785xIx7"));
        let secondary = first.secondary_ap.clone().unwrap();
        let mut fpb = FpbMock::with_measured_m4_fpb(&[(0, 0x5508_04D9)]);

        first.normalize_secondary_fpb_if_due(&mut fpb, &secondary, false);
        assert_eq!(fpb.comparator(0), 0);

        // What the crashed session left: a value nothing can decode.
        fpb.memory.insert(FP_COMP0, 0x0000_0001);

        let second = N32H7xx::create_for_chip(&dual_core("N32H785xIx7"));
        second.normalize_secondary_fpb_if_due(&mut fpb, &secondary, false);
        assert_eq!(fpb.comparator(0), 0, "the next session must heal it");
    }

    /// The second core's AP, as `create_for_chip` resolves it for the vendor's
    /// dual-core layout.
    fn secondary_ap() -> FullyQualifiedApAddress {
        N32H7xx::create_for_chip(&dual_core("N32H785xIx7"))
            .secondary_ap
            .clone()
            .expect("the dual-core description has a second AP")
    }

    #[test]
    fn fp_num_code_reads_the_count_out_of_either_revision() {
        // Revision 0: the M4's measured `FP_CTRL`, six comparators.
        assert_eq!(N32H7xx::fpb_num_code(0x0000_0261), Some(6));
        // Revision 1: the M7's, with the count split across two bit ranges —
        // eight comparators.
        assert_eq!(N32H7xx::fpb_num_code(0x1000_0081), Some(8));
        // Anything else is a layout with no safe way to walk it.
        assert_eq!(N32H7xx::fpb_num_code(0x2000_0081), None);
    }

    #[test]
    fn the_entry_breakpoint_takes_the_first_free_comparator() {
        // The regression this pins: the entry breakpoint used to be armed in
        // comparator 0 unconditionally — overwriting whatever
        // `set_hw_breakpoint` (which allocates from index 0 upwards) had put
        // there — and the disarm then zeroed it. Measured across a debugger
        // `restart` on an N32H785: the M7's first user breakpoint went from
        // 0x1500_04E7 to 0.
        let ap = secondary_ap();

        // A measured mid-session layout: two user breakpoints in the first two
        // comparators, the rest free.
        let mut fpb = FpbMock::with_measured_m4_fpb(&[(0, 0x5508_04D9), (1, 0x9508_04ED)]);
        assert_eq!(
            N32H7xx::free_fpb_comparator(&mut fpb, &ap, 0x261).expect("a readable FPB"),
            Some(2)
        );

        // A comparator holding an undecodable value is not free: overwriting
        // it would hide what the next normalization is looking for.
        fpb.memory.insert(FP_COMP0 + 8, 0x0000_0001);
        assert_eq!(
            N32H7xx::free_fpb_comparator(&mut fpb, &ap, 0x261).expect("a readable FPB"),
            Some(3)
        );

        // A full unit has no room, and the caller has to fall back to the
        // explicit halt.
        for index in 0..6u32 {
            fpb.memory
                .insert(FP_COMP0 + u64::from(index) * 4, 0x1000_0001);
        }
        assert_eq!(
            N32H7xx::free_fpb_comparator(&mut fpb, &ap, 0x261).expect("a readable FPB"),
            None
        );
    }

    #[test]
    fn a_snapshot_puts_the_breakpoints_back_after_a_reset() {
        // The other half of the same regression: a reset of the second core
        // leaves its FPB blank, and the session's breakpoints have to come
        // back, or the core is never stopped by them again.
        let ap = secondary_ap();
        let mut fpb = FpbMock::with_measured_m4_fpb(&[(0, 0x5508_04D9), (1, 0x9508_04ED)]);

        let snapshot = N32H7xx::snapshot_secondary_fpb(&mut fpb, &ap)
            .expect("a readable FPB")
            .expect("a revision-0 layout is walkable");

        // The entry breakpoint, armed after the snapshot, in the first free
        // comparator — the one the snapshot recorded as zero.
        fpb.memory.insert(FP_COMP0 + 8, 0x1508_0401);

        // The reset: the unit comes back as fresh hardware state. Its
        // configuration fields survive (they are read-only on silicon), ENABLE
        // does not.
        fpb.memory.clear();
        fpb.memory.insert(FP_CTRL, 0x260);

        N32H7xx::restore_secondary_fpb(&mut fpb, &ap, &snapshot).expect("a writable FPB");

        assert_eq!(
            fpb.comparator(0),
            0x5508_04D9,
            "the first user breakpoint must come back"
        );
        assert_eq!(
            fpb.comparator(1),
            0x9508_04ED,
            "the second user breakpoint must come back"
        );
        assert_eq!(fpb.comparator(2), 0, "the entry breakpoint must not");
        assert_eq!(
            fpb.memory.get(&FP_CTRL),
            Some(&0x261),
            "the unit must be enabled again (KEY does not read back)"
        );
    }

    #[test]
    fn a_snapshot_refuses_a_layout_it_cannot_walk() {
        let ap = secondary_ap();
        let mut fpb = FpbMock::with_measured_m4_fpb(&[]);
        fpb.memory.insert(FP_CTRL, 0x2000_0260);

        assert!(
            N32H7xx::snapshot_secondary_fpb(&mut fpb, &ap)
                .expect("a readable FPB")
                .is_none(),
            "an unknown FPB revision must not be captured; the caller falls back to clearing"
        );
    }
}
