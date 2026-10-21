//! Sequences for NSING N32H7xx series devices.
//!
//! # System reset
//!
//! A `SYSRESETREQ` write to AIRCR reports a FAULT, because the AHB-AP stops
//! answering while the reset runs. The vendor's CMSIS-Pack suppresses this
//! error too. The generic reset wait does not recover from a FAULT, so this
//! sequence recovers the connection itself. The order comes from the vendor's
//! J-Link script (`N32H7xx_CM7.jlinkscript`, `ResetTarget()`): clear the DP
//! sticky errors, clear `DEMCR.VC_CORERESET`, request the reset, wait, then
//! reconnect if the AP does not answer.
//!
//! `VC_CORERESET` must be clear. `reset_and_halt` sets it before it calls the
//! sequence, and `SYSRESETREQ` with the bit set kills the AHB-AP until a power
//! cycle: vector catch halts the core in the boot window, SWD is disabled
//! during boot, and boot never completes. The DP still answers, but the AP
//! does not. A reset without the bit set survives the same write. The SEGGER
//! script also says not to use this bit. Instead, an FPB breakpoint on the
//! application entry point tries to halt the core, and a halt request at the
//! end of the reset makes sure that it is halted.
//!
//! If the AP does not come back, the core stays halted where it is. This is
//! sufficient to flash.
//!
//! # Dual-core 78x parts
//!
//! 73x and 76x parts have one Cortex-M7. 78x parts also have a Cortex-M4 on
//! AP2 (`__ap="2"` in the vendor PDSC). `RCC->M4RSTREL` gates the M4 and its
//! AP: AP2 answers only while the M4 is released. The reset value is 0, so
//! the M4 is gated after power-up and after every `SYSRESETREQ`.
//!
//! A session fails if `debug_core_start` fails for one of its cores. Thus
//! `debug_core_start` skips the M4, and `on_attach` releases it when a
//! session uses it.
//!
//! Do not release the M4 earlier. A released M4 starts to fetch from
//! `MMU->M4_BOOT_ADDR` immediately. If Bank2 is blank, the M4 faults and the
//! M7 locks up too. Measured on an N32H785, in the ROM call of a flash
//! algorithm: `CFSR=0x401` (IACCVIOL, IMPRECISERR), `HFSR=0x40000002` (FORCED,
//! VECTTBL), `DHCSR=0x00090001` (`S_LOCKUP`). The fault cannot go to a
//! handler, because `VTOR` is still 0. With a single-core chip description,
//! the same probe and binary flash the part correctly.
//!
//! The release follows the vendor's `RCC_EnableCM4()`: first set
//! `MMU->M4_BOOT_ADDR`, then set `M4RSTREL`. The vendor PDSC sequence
//! (`releaseM4AndHaltM7`) does only the second step, so after power-up the M4
//! fetches its vectors from address 0.
//!
//! The M4 FPB is also in the gated domain. After a system reset its
//! comparators read zero, so the reset of the M4 captures them before and
//! writes them back after (`snapshot_secondary_fpb`). For the same reason the
//! M4 entry breakpoint is set after the reset. Then a `VECTRESET` restarts
//! only the M4 core, and the breakpoint halts it on its first instruction. A
//! halt request is too late: measured, the M4 gets to its main loop first.

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
        dp::{Abort, DpAddress, DpRegister},
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, DefaultArmSequence},
    },
    config::MemoryApExt,
};

const CPUID: u64 = 0xE000_ED00;
/// CPUID of the Cortex-M7 (r1p2).
const EXPECTED_CPUID: u32 = 0x411F_C272;
/// CPUID of the 78x Cortex-M4, measured.
const EXPECTED_CPUID_M4: u32 = 0x410F_C241;

/// `RCC->M4RSTREL`. Reset value is 0, which keeps the M4 in reset.
const M4RSTREL: u64 = 0x5803_0174;
/// `RCC_M4RSTREL_EN` in the vendor CMSIS header, `RCC_RSTEN_M4REL` in the RCC
/// driver header.
const M4RSTREL_M4REL: u32 = 1 << 0;
/// `MMU->M4_BOOT_ADDR`: the M4 vector table address. Accessed through the M7
/// AP, as the vendor code does.
const M4_BOOT_ADDR: u64 = 0x5110_50AC;
/// The M4 flash bank, which the vendor dual-core examples give to
/// `RCC_EnableCM4()`.
const M4_BOOT_ADDR_DEFAULT: u32 = 0x1508_0000;
/// Time for AP2 to answer after the M4 release.
const M4_RELEASE_TIMEOUT: Duration = Duration::from_millis(200);

/// A `CSW` for 32-bit transfers with single address increment. Measured: a
/// probe-rs session sets this value on both APs of an N32H785.
const CSW_WORD_ACCESS: u32 = 0x2300_0052;

/// Wait after the reset request before AP access. SEGGER waits 600 ms, the
/// vendor CMSIS-Pack 500 ms.
const RESET_SETTLE: Duration = Duration::from_millis(600);
/// Time to recover the AP after the reset.
const RECOVERY_TIMEOUT: Duration = Duration::from_millis(1000);
/// Time for the core to acknowledge a halt request.
const HALT_TIMEOUT: Duration = Duration::from_millis(200);
/// Time for `DHCSR.S_RESET_ST` to clear after a `VECTRESET`.
const VECTRESET_TIMEOUT: Duration = Duration::from_millis(100);

/// `MMU->M7_StoreAddr_cur`: the M7 code address. Same in the 73x, 76x and 78x
/// `ResetSystem` sequences.
const M7_STORE_ADDR_CUR: u64 = 0x5110_511C;
const FP_CTRL: u64 = 0xE000_2000;
const FP_CTRL_ENABLE: u32 = 1 << 0;
/// Each write to `FP_CTRL` must set this bit.
const FP_CTRL_KEY: u32 = 1 << 1;
const FP_COMP0: u64 = 0xE000_2008;

/// Address of FPB comparator `index`.
fn fp_comp(index: u32) -> u64 {
    FP_COMP0 + u64::from(index) * 4
}

/// Initialization sequencing for N32H7xx parts.
#[derive(Debug)]
pub struct N32H7xx {
    /// The AP of the first core in the chip description: the Cortex-M7.
    primary_ap: FullyQualifiedApAddress,
    /// The AP of the Cortex-M4. `None` on single-core parts, which makes all
    /// dual-core code paths no-ops.
    secondary_ap: Option<FullyQualifiedApAddress>,
    /// Set by the first M4 attach of the session, also if the FPB clear
    /// fails. See `normalize_secondary_fpb_if_due`.
    secondary_fpb_sanitized: AtomicBool,
}

/// The entry breakpoint that `arm_entry_breakpoint` set.
#[derive(Debug)]
struct ArmedEntryBreakpoint {
    /// The comparator that holds the breakpoint.
    index: u32,
    /// The `FP_CTRL` ENABLE bit before the breakpoint was set.
    was_enabled: bool,
}

/// The M4 FPB state before a reset.
#[derive(Debug)]
struct FpbSnapshot {
    /// The `FP_CTRL` ENABLE bit. The other fields are read-only.
    enabled: bool,
    /// All comparator values, `FP_COMP0` first.
    comparators: Vec<u32>,
}

impl N32H7xx {
    /// Create the sequencer for the N32H7xx series.
    ///
    /// Cores are identified by AP, not by name: the vendor PDSC sets the AP
    /// (`__ap="0"` / `__ap="2"`), but core names are free text.
    pub fn create_for_chip(chip: &Chip) -> Arc<Self> {
        let mut aps = chip.cores.iter().filter_map(MemoryApExt::memory_ap);

        // The vendor match is by name, so do not panic on a chip without an
        // ARM memory AP.
        let primary_ap = aps
            .next()
            .unwrap_or_else(|| FullyQualifiedApAddress::v1_with_dp(DpAddress::Default, 0));

        let secondary_ap = aps.find(|ap| *ap != primary_ap);

        Arc::new(Self {
            primary_ap,
            secondary_ap,
            secondary_fpb_sanitized: AtomicBool::new(false),
        })
    }

    fn is_secondary(&self, ap: &FullyQualifiedApAddress) -> bool {
        self.secondary_ap.as_ref() == Some(ap)
    }

    /// The CPUID that the core on this AP must report. `None` for an unknown
    /// AP.
    fn expected_cpuid(&self, ap: &FullyQualifiedApAddress) -> Option<u32> {
        if self.is_secondary(ap) {
            Some(EXPECTED_CPUID_M4)
        } else if *ap == self.primary_ap {
            Some(EXPECTED_CPUID)
        } else {
            None
        }
    }

    /// Set the AP to 32-bit transfers, if it is not.
    ///
    /// `CSW.Size` stays in the hardware between sessions, and the raw access
    /// in this sequence does not set it. After an 8-bit transfer (for example
    /// the stack check of a flash algorithm), a raw read of `CPUID` returns
    /// one byte and old `DRW` data.
    /// Measured: with `CSW=0x23000050` the M4 `CPUID` read was wrong, and the
    /// attach failed with "Core 1 is not enabled".
    fn ensure_word_access<D: DapAccess + ?Sized>(
        interface: &mut D,
        ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let csw = interface.read_raw_ap_register(ap, CSW::ADDRESS)?;
        if (csw & 0b111) == (CSW_WORD_ACCESS & 0b111) {
            return Ok(());
        }

        tracing::info!("N32H7xx: AP {ap:?} has CSW=0x{csw:08X}, writing 0x{CSW_WORD_ACCESS:08X}");
        interface.write_raw_ap_register(ap, CSW::ADDRESS, CSW_WORD_ACCESS)?;

        let readback = interface.read_raw_ap_register(ap, CSW::ADDRESS)?;
        if (readback & 0b111) != (CSW_WORD_ACCESS & 0b111) {
            tracing::warn!("N32H7xx: AP {ap:?} still has CSW=0x{readback:08X} after the write");
            return Err(ArmError::Timeout);
        }
        Ok(())
    }

    /// Read a word through the raw AP registers.
    ///
    /// This does not use a cached `CSW` value, because a reset can make the
    /// cache incorrect.
    fn read_word<D: DapAccess + ?Sized>(
        interface: &mut D,
        ap: &FullyQualifiedApAddress,
        address: u64,
    ) -> Result<u32, ArmError> {
        Self::ensure_word_access(interface, ap)?;
        interface.write_raw_ap_register(ap, TAR::ADDRESS, address as u32)?;
        interface.read_raw_ap_register(ap, DRW::ADDRESS)
    }

    /// Write a word through the raw AP registers. See `read_word`.
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

    /// Clear the DP sticky error bits.
    ///
    /// On SWD, a failed AP access blocks all subsequent accesses until ABORT
    /// clears the error.
    fn clear_sticky_errors<D: DapAccess + ?Sized>(interface: &mut D, dp: DpAddress) {
        let mut abort = Abort(0);
        abort.set_orunerrclr(true);
        abort.set_wderrclr(true);
        abort.set_stkerrclr(true);
        abort.set_stkcmpclr(true);

        if let Err(e) = interface.write_raw_dp_register(dp, Abort::ADDRESS, u32::from(abort)) {
            tracing::debug!("N32H7xx: could not clear the DP sticky errors: {e}");
        }
    }

    /// Set `MMU->M4_BOOT_ADDR` to the default if its value cannot be a vector
    /// table address.
    ///
    /// A valid value stays: the firmware knows where its vector table is.
    /// After power-up the register is 0.
    fn point_m4_at_its_vector_table<D: DapAccess + ?Sized>(
        iface: &mut D,
        ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let boot_addr = Self::read_word(iface, ap, M4_BOOT_ADDR)?;
        if boot_addr != 0 && boot_addr % 4 == 0 {
            tracing::debug!("N32H7xx: MMU->M4_BOOT_ADDR is 0x{boot_addr:08X}");
            return Ok(());
        }

        tracing::info!(
            "N32H7xx: MMU->M4_BOOT_ADDR is 0x{boot_addr:08X}, setting it to \
             0x{M4_BOOT_ADDR_DEFAULT:08X}"
        );
        Self::write_word(iface, ap, M4_BOOT_ADDR, M4_BOOT_ADDR_DEFAULT)
    }

    /// Release the M4 from reset as `RCC_EnableCM4()` does, then wait for its
    /// AP. A no-op on single-core parts.
    ///
    /// This does not halt the M7. The vendor PDSC halts the M7 for M4
    /// downloads, to stop its DMA. A connection to the M4 must not stop the
    /// M7, so only the reset path does that.
    ///
    /// Returns `true` if this call released the core. A core that was already
    /// running must not be halted unexpectedly.
    fn release_secondary_core<D: DapAccess + ?Sized>(
        &self,
        iface: &mut D,
    ) -> Result<bool, ArmError> {
        let Some(secondary_ap) = self.secondary_ap.as_ref() else {
            return Ok(false);
        };

        // Use the M7 AP, as the vendor code does. The M4 AP depends on the
        // gate that this function changes.
        let ap = self.primary_ap.clone();

        Self::point_m4_at_its_vector_table(iface, &ap)?;

        let mut released_now = false;
        let rstrel = Self::read_word(iface, &ap, M4RSTREL)?;
        if rstrel & M4RSTREL_M4REL == 0 {
            tracing::info!("N32H7xx: releasing the M4 (RCC->M4RSTREL=0x{rstrel:08X})");
            Self::write_word(iface, &ap, M4RSTREL, rstrel | M4RSTREL_M4REL)?;

            let readback = Self::read_word(iface, &ap, M4RSTREL)?;
            if readback & M4RSTREL_M4REL == 0 {
                tracing::warn!("N32H7xx: RCC->M4RSTREL is 0x{readback:08X} after the write");
                return Err(ArmError::Timeout);
            }
            released_now = true;
        }

        // AP2 answers some time after the release.
        let deadline = Instant::now() + M4_RELEASE_TIMEOUT;
        loop {
            match iface.read_raw_ap_register(secondary_ap, IDR::ADDRESS) {
                Ok(idr) if idr != 0 => {
                    tracing::debug!("N32H7xx: M4 AP is up (IDR=0x{idr:08X})");
                    return Ok(released_now);
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!("N32H7xx: M4 AP does not answer yet: {e}");
                    Self::clear_sticky_errors(iface, ap.dp());
                }
            }

            if Instant::now() >= deadline {
                // Not fatal: the M7 is still usable.
                tracing::warn!(
                    "N32H7xx: M4 AP did not come up within {M4_RELEASE_TIMEOUT:?} of the release"
                );
                return Ok(released_now);
            }

            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Release the M4 again after a system reset. Returns `true` if the M4 AP
    /// is usable.
    ///
    /// No access through the M4 AP is permitted before the release. Measured
    /// with a ULINK2, which does not retry internally: a memory read through
    /// the gated AP gets no answer, the probe waits for its full USB timeout,
    /// and the queued DP operations fail too. The release uses the M7 AP and
    /// can be repeated, so it is retried until the timeout.
    fn rerelease_secondary_core(&self, arm: &mut dyn ArmDebugInterface) -> bool {
        let Some(secondary_ap) = self.secondary_ap.as_ref() else {
            return false;
        };

        let deadline = Instant::now() + RECOVERY_TIMEOUT;
        loop {
            match self.release_secondary_core(arm) {
                Ok(_) => break,
                Err(e) if Instant::now() >= deadline => {
                    tracing::warn!("N32H7xx: could not release the M4 again: {e}");
                    return false;
                }
                Err(e) => {
                    tracing::debug!("N32H7xx: releasing the M4 again failed: {e}");
                    Self::clear_sticky_errors(arm, self.primary_ap.dp());
                    thread::sleep(Duration::from_millis(25));
                }
            }
        }

        // A valid IDR shows only that the AP has power. Also check memory
        // access.
        self.ap_is_alive(arm, secondary_ap)
    }

    /// Check that memory reads and writes through the AP work.
    ///
    /// The `DRW` write is necessary because a write reports the FAULT during
    /// a reset.
    fn ap_is_alive(&self, arm: &mut dyn ArmDebugInterface, ap: &FullyQualifiedApAddress) -> bool {
        let cpuid = match Self::read_word(arm, ap, CPUID) {
            Ok(cpuid) => cpuid,
            Err(e) => {
                // Usual right after a reset, so not a warning.
                tracing::debug!("N32H7xx: AP {ap:?} did not answer a CPUID read: {e}");
                return false;
            }
        };

        match self.expected_cpuid(ap) {
            Some(expected) if cpuid != expected => {
                // Usually a truncated read (`CSW` not set to 32 bits).
                tracing::warn!(
                    "N32H7xx: AP {ap:?} has CPUID 0x{cpuid:08X}, expected 0x{expected:08X}"
                );
                return false;
            }
            // Unknown AP: accept any Cortex-M CPUID.
            None if cpuid >> 24 != 0x41 || cpuid & 0x000F_FFF0 == 0 => {
                tracing::debug!("N32H7xx: AP {ap:?} has CPUID 0x{cpuid:08X}, not a Cortex-M");
                return false;
            }
            _ => {}
        }

        // Write back the same DEMCR value to make sure that writes work. This
        // sequence owns DEMCR.
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
                tracing::debug!("N32H7xx: AP {ap:?} did not accept a DEMCR write: {e}");
                false
            }
        }
    }

    /// Halt the core and wait for `DHCSR.S_HALT`.
    ///
    /// This reports a failed halt here, near its cause, not later as a
    /// timeout of the caller.
    fn halt_and_confirm(
        &self,
        arm: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let deadline = Instant::now() + HALT_TIMEOUT;
        let mut halt = Dhcsr(0);
        halt.enable_write();
        halt.set_c_debugen(true);
        halt.set_c_halt(true);

        loop {
            let dhcsr = Dhcsr(Self::read_word(arm, ap, Dhcsr::get_mmio_address())?);
            if dhcsr.s_halt() {
                return Ok(());
            }

            Self::write_word(arm, ap, Dhcsr::get_mmio_address(), halt.into())?;

            if Instant::now() >= deadline {
                return Err(ArmError::Timeout);
            }

            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Restart a halted core with `AIRCR.VECTRESET`, as the vendor
    /// `ResetProcessor` sequence does.
    ///
    /// The core must be halted: the ARM ARM says the effect of `VECTRESET` is
    /// UNPREDICTABLE if the core is not in Debug state. Unlike the
    /// `M4RSTREL` gate, this reset keeps the debug domain and the FPB.
    ///
    /// `DHCSR.S_RESET_ST` is sticky and a `DHCSR` read clears it.
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

    /// Set an FPB breakpoint on the application entry point.
    ///
    /// The vendor `ResetSystem` sequences do this instead of
    /// `DEMCR.VC_CORERESET`. Each core reads the vector table address from its
    /// own register: `M7_StoreAddr_cur` for the M7, `M4_BOOT_ADDR` for the M4.
    ///
    /// The breakpoint goes into the first comparator that is zero, so the
    /// session's breakpoints stay.
    ///
    /// Best effort: on failure this returns `None`, and the explicit halt at
    /// the end of `reset_system` stops the core.
    fn arm_entry_breakpoint(
        &self,
        arm: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
    ) -> Option<ArmedEntryBreakpoint> {
        let (boot_addr_register, boot_addr_name) = if self.is_secondary(ap) {
            (M4_BOOT_ADDR, "MMU->M4_BOOT_ADDR")
        } else {
            (M7_STORE_ADDR_CUR, "M7_StoreAddr_cur")
        };

        let Ok(boot_addr) = Self::read_word(arm, ap, boot_addr_register) else {
            tracing::debug!("N32H7xx: could not read {boot_addr_name}, no entry breakpoint");
            return None;
        };

        // The vendor reads this address without a check. A wrong address
        // causes a bus fault on the next read.
        if boot_addr == 0 || boot_addr % 4 != 0 {
            tracing::debug!("N32H7xx: {boot_addr_name} is 0x{boot_addr:08X}, no entry breakpoint");
            return None;
        }

        let Ok(entry) = Self::read_word(arm, ap, u64::from(boot_addr) + 4) else {
            tracing::debug!("N32H7xx: could not read the reset vector, no entry breakpoint");
            return None;
        };

        // A Thumb reset vector has bit 0 set. This rejects blank flash
        // (0xFFFF_FFFF) and zero. A blank part locks up before any entry
        // point, so the explicit halt stops it.
        if entry == u32::MAX || entry & 1 == 0 {
            tracing::debug!("N32H7xx: reset vector is 0x{entry:08X}, no entry breakpoint");
            return None;
        }

        // The M4 has an FpRev1 unit and the M7 an FpRev2 unit. They encode
        // the address differently.
        let comparator_value = if self.is_secondary(ap) {
            let Some(value) = Self::fp_rev1_comparator_value(entry) else {
                tracing::debug!(
                    "N32H7xx: entry point 0x{entry:08X} is out of FpRev1 range, no entry \
                     breakpoint"
                );
                return None;
            };
            value
        } else {
            (entry & 0x1FFF_FFFC) | 0x01
        };

        let Ok(ctrl) = Self::read_word(arm, ap, FP_CTRL) else {
            tracing::debug!("N32H7xx: could not read FP_CTRL, no entry breakpoint");
            return None;
        };
        let index = match Self::free_fpb_comparator(arm, ap, ctrl) {
            Ok(Some(index)) => index,
            Ok(None) => {
                tracing::debug!("N32H7xx: no free FPB comparator, no entry breakpoint");
                return None;
            }
            Err(e) => {
                tracing::debug!("N32H7xx: could not read the FPB comparators: {e}");
                return None;
            }
        };

        let comparator = fp_comp(index);
        if let Err(e) = Self::write_word(arm, ap, comparator, comparator_value) {
            tracing::debug!("N32H7xx: could not set the FPB breakpoint: {e}");
            return None;
        }

        if let Err(e) = Self::write_word(arm, ap, FP_CTRL, FP_CTRL_KEY | FP_CTRL_ENABLE) {
            tracing::debug!("N32H7xx: could not enable the FPB: {e}");
            // Do not leave a comparator set on a disabled unit. It looks like
            // a user breakpoint later.
            if let Err(e) = Self::write_word(arm, ap, comparator, 0) {
                tracing::debug!("N32H7xx: could not clear the comparator: {e}");
            }
            return None;
        }

        tracing::debug!(
            "N32H7xx: entry breakpoint at 0x{:08X} (comparator {index})",
            entry & !0b1
        );

        Some(ArmedEntryBreakpoint {
            index,
            was_enabled: ctrl & FP_CTRL_ENABLE != 0,
        })
    }

    /// Remove the entry breakpoint.
    ///
    /// This clears only its comparator and restores the `FP_CTRL` ENABLE bit.
    /// If the unit is disabled, the other comparators do not halt the core.
    fn disarm_entry_breakpoint(
        &self,
        arm: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
        armed: &ArmedEntryBreakpoint,
    ) {
        if let Err(e) = Self::write_word(arm, ap, fp_comp(armed.index), 0) {
            tracing::debug!("N32H7xx: could not clear the FPB breakpoint: {e}");
        }
        let ctrl = FP_CTRL_KEY | u32::from(armed.was_enabled);
        if let Err(e) = Self::write_word(arm, ap, FP_CTRL, ctrl) {
            tracing::debug!("N32H7xx: could not restore FP_CTRL: {e}");
        }
    }

    /// The FpRev1 (M4) comparator value for a breakpoint on `entry`.
    ///
    /// FpRev1 selects the half-word with `REPLACE` (bits 31:30): `0b01` for
    /// the half-word at `...0`, `0b10` for the half-word at `...2`.
    /// `REPLACE = 0b00` is the remap encoding. The ARM ARM says it is
    /// UNPREDICTABLE on a unit without remap. `None` if the address is not in
    /// the code region (below `0x2000_0000`).
    fn fp_rev1_comparator_value(entry: u32) -> Option<u32> {
        // Bit 0 is the Thumb bit.
        let address = entry & !0b1;
        if address >= 0x2000_0000 {
            return None;
        }

        let replace = if address & 0b11 == 0 { 0b01 } else { 0b10 };
        Some((replace << 30) | (address & 0x1FFF_FFFC) | 0x01)
    }

    /// The number of code comparators, from `FP_CTRL`. Revision 1 splits the
    /// count into two fields. `None` for an unknown revision.
    fn fpb_num_code(ctrl: u32) -> Option<u32> {
        match ctrl >> 28 {
            0 => Some((ctrl >> 4) & 0xF),
            1 => Some((((ctrl >> 12) & 0x7) << 4) | ((ctrl >> 4) & 0xF)),
            _ => None,
        }
    }

    /// The first comparator that is zero. `None` if all are in use or the
    /// revision is unknown.
    ///
    /// A comparator with an invalid value is not free.
    /// `sanitize_secondary_fpb` clears such values.
    fn free_fpb_comparator<D: DapAccess + ?Sized>(
        iface: &mut D,
        ap: &FullyQualifiedApAddress,
        ctrl: u32,
    ) -> Result<Option<u32>, ArmError> {
        let Some(num_code) = Self::fpb_num_code(ctrl) else {
            tracing::debug!("N32H7xx: unknown FPB revision {}", ctrl >> 28);
            return Ok(None);
        };

        for index in 0..num_code {
            if Self::read_word(iface, ap, fp_comp(index))? == 0 {
                return Ok(Some(index));
            }
        }

        Ok(None)
    }

    /// Clear all non-zero M4 FPB comparators. `FP_CTRL` does not change.
    ///
    /// The reset values of the M4 comparators are unknown. Measured on an
    /// N32H785: `FP_COMP2 = 0x0000_0001` after an M4 reset. The probe-rs
    /// breakpoint code cannot decode this value, so it cannot clear it, and
    /// the attach fails. Then the debugger cannot repair the board. Thus this
    /// clears the comparators without a decode.
    fn sanitize_secondary_fpb<D: DapAccess + ?Sized>(
        iface: &mut D,
        ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let ctrl = Self::read_word(iface, ap, FP_CTRL)?;
        let Some(num_code) = Self::fpb_num_code(ctrl) else {
            tracing::debug!("N32H7xx: unknown M4 FPB revision {}", ctrl >> 28);
            return Ok(());
        };

        for index in 0..num_code {
            let value = Self::read_word(iface, ap, fp_comp(index))?;
            if value != 0 {
                tracing::info!("N32H7xx: clearing M4 FPB comparator {index} (0x{value:08X})");
                Self::write_word(iface, ap, fp_comp(index), 0)?;
            }
        }

        Ok(())
    }

    /// Read the M4 FPB state before a reset of the M4, so
    /// `restore_secondary_fpb` can write it back. `None` for an unknown
    /// revision.
    fn snapshot_secondary_fpb<D: DapAccess + ?Sized>(
        iface: &mut D,
        ap: &FullyQualifiedApAddress,
    ) -> Result<Option<FpbSnapshot>, ArmError> {
        let ctrl = Self::read_word(iface, ap, FP_CTRL)?;

        let Some(num_code) = Self::fpb_num_code(ctrl) else {
            tracing::debug!("N32H7xx: unknown M4 FPB revision {}", ctrl >> 28);
            return Ok(None);
        };

        let comparators = (0..num_code)
            .map(|index| Self::read_word(iface, ap, fp_comp(index)))
            .collect::<Result<Vec<_>, _>>()?;

        tracing::debug!("N32H7xx: M4 FPB: FP_CTRL=0x{ctrl:08X}, comparators={comparators:X?}");

        Ok(Some(FpbSnapshot {
            enabled: ctrl & FP_CTRL_ENABLE != 0,
            comparators,
        }))
    }

    /// Write back the M4 FPB state from `snapshot_secondary_fpb`.
    ///
    /// `FP_CTRL` is written last, so that the unit does not match on a
    /// partial set of comparators.
    fn restore_secondary_fpb<D: DapAccess + ?Sized>(
        iface: &mut D,
        ap: &FullyQualifiedApAddress,
        snapshot: &FpbSnapshot,
    ) -> Result<(), ArmError> {
        for (index, value) in (0..).zip(&snapshot.comparators) {
            Self::write_word(iface, ap, fp_comp(index), *value)?;
        }

        Self::write_word(
            iface,
            ap,
            FP_CTRL,
            FP_CTRL_KEY | u32::from(snapshot.enabled),
        )?;

        tracing::debug!("N32H7xx: restored the M4 FPB");
        Ok(())
    }

    /// Clear the M4 FPB on the first attach of a session and on an attach that
    /// released the M4.
    ///
    /// `on_attach` can run before each access to the core. On the other
    /// attaches the comparators hold the session's breakpoints. A clear there
    /// removes breakpoints that the debugger reports as set, and the core does
    /// not stop on them.
    ///
    /// On the first attach no breakpoints are set yet, and an invalid value
    /// must go before the session reads the comparators. After a release, the
    /// comparators contain hardware reset values, not breakpoints.
    ///
    /// The latch is per instance, so a new sequence clears again. `Relaxed`
    /// is sufficient because the attaches of a session are serial.
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
            tracing::warn!("N32H7xx: could not clear the M4 FPB: {e}");
            Self::clear_sticky_errors(iface, ap.dp());
        }
    }

    /// Write AIRCR to request a reset.
    ///
    /// The write usually fails because the reset removes the AP. The vendor
    /// `ResetSystem` sequence ignores this error too.
    fn request_reset(
        &self,
        arm: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
        value: u32,
    ) {
        if let Err(e) = Self::write_word(arm, ap, Aircr::get_mmio_address(), value) {
            tracing::debug!("N32H7xx: AIRCR write failed as expected: {e}");
        }
    }
}

impl ArmDebugSequence for N32H7xx {
    /// Release the M4 when a session attaches to it. See the module
    /// documentation.
    fn on_attach(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        core_type: CoreType,
        target: &mut crate::Target,
    ) -> Result<(), ArmError> {
        if self.is_secondary(core_ap) {
            let released_now = match self.release_secondary_core(interface) {
                Ok(released_now) => released_now,
                Err(e) => {
                    tracing::warn!("N32H7xx: could not release the M4: {e}");
                    Self::clear_sticky_errors(interface, core_ap.dp());
                    return Err(ArmError::CoreDisabled);
                }
            };

            // The M4 is at its first instruction. If Bank2 is blank, it
            // faults and the M7 locks up too.
            if released_now && let Err(e) = self.halt_and_confirm(interface, core_ap) {
                tracing::warn!("N32H7xx: the M4 did not halt after the release: {e}");
                return Err(ArmError::CoreDisabled);
            }

            if !self.ap_is_alive(interface, core_ap) {
                tracing::warn!("N32H7xx: M4 AP does not answer after the release");
                return Err(ArmError::CoreDisabled);
            }

            self.normalize_secondary_fpb_if_due(interface, core_ap, released_now);
        }

        DefaultArmSequence(()).on_attach(interface, core_ap, core_type, target)
    }

    /// Skip the M4, which can be in reset when the session starts.
    /// `on_attach` releases it.
    fn debug_core_start(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        core_type: CoreType,
        debug_base: Option<u64>,
        cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        if self.is_secondary(core_ap) {
            tracing::debug!("N32H7xx: skipping debug setup for the M4");
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

        // `reset_and_halt` sets DEMCR.VC_CORERESET, `Core::reset` does not.
        // This tells if the caller expects a halted core.
        let demcr_raw = interface.read_word_32(Demcr::get_mmio_address()).ok();
        let halt_after = demcr_raw.is_some_and(|value| Demcr(value).vc_corereset());

        // Clear VC_CORERESET before the reset. See the module documentation.
        if let Some(raw) = demcr_raw {
            let mut demcr = Demcr(raw);
            if demcr.vc_corereset() {
                demcr.set_vc_corereset(false);
                if let Err(e) = interface.write_word_32(Demcr::get_mmio_address(), demcr.into()) {
                    tracing::warn!(
                        "N32H7xx: could not clear DEMCR.VC_CORERESET ({e}), the AHB-AP can \
                         stop after the reset"
                    );
                }
            }
        }

        tracing::info!("N32H7xx: reset (halt_after={halt_after})");

        let arm = interface.get_arm_debug_interface()?;
        let resetting_secondary = self.is_secondary(&ap);

        // The reset clears the M4 FPB. Read it before the reset, to restore
        // the session's breakpoints after it.
        let fpb_snapshot = if resetting_secondary {
            Self::snapshot_secondary_fpb(arm, &ap).unwrap_or_else(|e| {
                tracing::warn!("N32H7xx: could not read the M4 FPB ({e}), M4 breakpoints are lost");
                Self::clear_sticky_errors(arm, ap.dp());
                None
            })
        } else {
            None
        };

        // `Core::reset` must not halt the core. The M4 FPB does not survive
        // the reset, so the M4 entry breakpoint is set after it.
        let mut entry_breakpoint = None;
        if halt_after && !resetting_secondary {
            entry_breakpoint = self.arm_entry_breakpoint(arm, &ap);
        }

        // Immediately before the AIRCR write, as SEGGER does. The entry
        // breakpoint reads MMU registers, which can set sticky errors.
        Self::clear_sticky_errors(arm, ap.dp());

        // SYSRESETREQ, as the vendor `ResetSystem` does. VECTRESET does not
        // reset peripherals, clocks or the flash controller.
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        self.request_reset(arm, &ap, aircr.into());

        thread::sleep(RESET_SETTLE);

        // On an M4 reset, wait for the M7 AP: the M4 AP is gated until the
        // release, and a read through it can stall the probe (see
        // `rerelease_secondary_core`).
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

            Self::clear_sticky_errors(arm, watch.dp());

            // Requests the debug power again, which the reset can remove.
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
                "N32H7xx: AP {watch:?} did not come back after the reset; flashing can \
                 continue"
            );
        }

        // The system reset gates the M4 again. Release it only on an M4 reset:
        // a release after an M7 reset breaks M7 flashing, and overrides
        // firmware that keeps the M4 in reset.
        let secondary_usable = resetting_secondary && self.rerelease_secondary_core(arm);

        if secondary_usable {
            // Halt the M4 at once: if Bank2 is blank, the M4 faults and the M7
            // locks up too. The M7 halt below can take hundreds of
            // milliseconds.
            if halt_after && let Err(e) = self.halt_and_confirm(arm, &ap) {
                tracing::warn!("N32H7xx: the M4 did not halt after the release: {e}");
                Self::clear_sticky_errors(arm, ap.dp());
            }

            // Restore the session's breakpoints. Without a snapshot, clear the
            // comparators so that the next breakpoint walk does not fail.
            match &fpb_snapshot {
                Some(snapshot) => {
                    if let Err(e) = Self::restore_secondary_fpb(arm, &ap, snapshot) {
                        tracing::warn!("N32H7xx: could not restore the M4 FPB: {e}");
                        Self::clear_sticky_errors(arm, ap.dp());
                    }
                }
                None => {
                    if let Err(e) = Self::sanitize_secondary_fpb(arm, &ap) {
                        tracing::debug!("N32H7xx: could not clear the M4 FPB: {e}");
                        Self::clear_sticky_errors(arm, ap.dp());
                    }
                }
            }

            // Set the entry breakpoint, then restart the M4 with VECTRESET so
            // that the breakpoint halts it on its first instruction.
            if halt_after {
                match self.arm_entry_breakpoint(arm, &ap) {
                    Some(armed) => match self.vectreset(arm, &ap) {
                        Ok(()) => entry_breakpoint = Some(armed),
                        Err(e) => {
                            tracing::warn!(
                                "N32H7xx: M4 VECTRESET failed ({e}), the M4 stays halted \
                                 after the release"
                            );
                            self.disarm_entry_breakpoint(arm, &ap, &armed);
                            Self::clear_sticky_errors(arm, ap.dp());
                        }
                    },
                    // Without a breakpoint, a restart loses the halt.
                    None => tracing::debug!("N32H7xx: the M4 stays halted after the release"),
                }
            }

            // The system reset also restarted the M7. Halt it, as the vendor
            // `releaseM4AndHaltM7` does for M4 downloads: an M7 that runs (DMA,
            // peripherals) causes M4 downloads to fail. `Core::reset` must not
            // halt either core.
            if halt_after && let Err(e) = self.halt_and_confirm(arm, &self.primary_ap) {
                tracing::warn!("N32H7xx: could not halt the M7 for the M4 reset: {e}");
                Self::clear_sticky_errors(arm, self.primary_ap.dp());
            }
        }

        // The reset disabled debug.
        if let Err(e) = self.debug_core_start(arm, &ap, core_type, debug_base, None) {
            tracing::debug!("N32H7xx: debug_core_start failed: {e}");
        }

        // If the entry breakpoint did not halt the core (for example, blank
        // flash locks up first), or the reset did not occur, halt it here.
        // Do not access a gated M4 AP.
        let core_usable = !resetting_secondary || secondary_usable;
        if core_usable
            && (halt_after || !alive)
            && let Err(e) = self.halt_and_confirm(arm, &ap)
        {
            tracing::warn!("N32H7xx: the core did not halt after the reset: {e}");
        }

        // Remove the entry breakpoint, or the next run stops on it.
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

    use crate::architecture::arm::ApAddress as ArmApAddress;
    use crate::architecture::arm::armv7m::FpRev1CompX;
    use crate::architecture::arm::dp::DpRegisterAddress;
    use probe_rs_target::{
        ApAddress as TargetApAddress, ArmCoreAccessOptions, Core, CoreAccessOptions,
    };

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

    /// The single-core 73x/76x parts must have no dual-core behavior.
    #[test]
    fn single_core_parts_have_no_second_core_to_release() {
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

            assert_eq!(sequence.expected_cpuid(secondary), Some(EXPECTED_CPUID_M4));
            assert_eq!(
                sequence.expected_cpuid(&sequence.primary_ap),
                Some(EXPECTED_CPUID)
            );
        }
    }

    #[test]
    fn cores_are_told_apart_by_ap_not_by_name() {
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
        assert_eq!(N32H7xx::fp_rev1_comparator_value(0x2000_0001), None);
        assert_eq!(N32H7xx::fp_rev1_comparator_value(0x2400_0001), None);
        assert!(
            FpRev1CompX::breakpoint_configuration(0x2000_0000).is_err(),
            "probe-rs's own encoder must refuse the same ceiling"
        );
    }

    /// An AP with raw `CSW`, `TAR` and `DRW` access to a flat memory.
    struct FpbMock {
        csw: u32,
        tar: u32,
        memory: HashMap<u64, u32>,
    }

    impl FpbMock {
        /// An M4 FPB with the given comparators. `FP_CTRL = 0x261` is the
        /// value measured on an N32H785: revision 0, six comparators.
        fn with_measured_m4_fpb(comparators: &[(u32, u32)]) -> Self {
            let mut memory = HashMap::new();
            memory.insert(FP_CTRL, 0x261);
            for (index, value) in comparators {
                memory.insert(fp_comp(*index), *value);
            }
            Self {
                csw: CSW_WORD_ACCESS,
                tar: 0,
                memory,
            }
        }

        fn comparator(&self, index: u32) -> u32 {
            self.memory.get(&fp_comp(index)).copied().unwrap_or(0)
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
                    // Only ENABLE is writable in `FP_CTRL`. KEY reads as 0.
                    // Measured: the comparator count stays after a write of
                    // 0x03.
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
        // Each attach of a session must not clear the breakpoints that an
        // earlier request set.
        let sequence = N32H7xx::create_for_chip(&dual_core("N32H785xIx7"));
        let secondary = sequence
            .secondary_ap
            .clone()
            .expect("the dual-core description has a second AP");

        // The measured invalid value.
        let mut fpb = FpbMock::with_measured_m4_fpb(&[(2, 0x0000_0001)]);

        sequence.normalize_secondary_fpb_if_due(&mut fpb, &secondary, false);
        assert_eq!(fpb.comparator(2), 0, "the invalid value must be cleared");
        assert_eq!(
            fpb.memory.get(&FP_CTRL),
            Some(&0x261),
            "FP_CTRL must not change"
        );

        fpb.write_raw_ap_register(&secondary, TAR::ADDRESS, FP_COMP0 as u32)
            .unwrap();
        fpb.write_raw_ap_register(&secondary, DRW::ADDRESS, 0x5508_04D9)
            .unwrap();

        for _ in 0..3 {
            sequence.normalize_secondary_fpb_if_due(&mut fpb, &secondary, false);
        }
        assert_eq!(
            fpb.comparator(0),
            0x5508_04D9,
            "the session's breakpoint must stay"
        );
    }

    #[test]
    fn an_attach_that_released_the_core_normalizes_again() {
        let sequence = N32H7xx::create_for_chip(&dual_core("N32H785xIx7"));
        let secondary = sequence.secondary_ap.clone().unwrap();
        let mut fpb = FpbMock::with_measured_m4_fpb(&[]);

        sequence.normalize_secondary_fpb_if_due(&mut fpb, &secondary, false);
        fpb.write_raw_ap_register(&secondary, TAR::ADDRESS, FP_COMP0 as u32)
            .unwrap();
        fpb.write_raw_ap_register(&secondary, DRW::ADDRESS, 0x5508_04D9)
            .unwrap();

        // A reset gated the core, and the comparators contain reset values.
        fpb.memory.insert(fp_comp(2), 0x0000_0001);
        sequence.normalize_secondary_fpb_if_due(&mut fpb, &secondary, true);
        assert_eq!(fpb.comparator(2), 0, "the invalid value must be cleared");
        assert_eq!(fpb.comparator(0), 0, "all values are reset values");
    }

    #[test]
    fn a_new_session_normalizes_the_fpb_again() {
        let first = N32H7xx::create_for_chip(&dual_core("N32H785xIx7"));
        let secondary = first.secondary_ap.clone().unwrap();
        let mut fpb = FpbMock::with_measured_m4_fpb(&[(0, 0x5508_04D9)]);

        first.normalize_secondary_fpb_if_due(&mut fpb, &secondary, false);
        assert_eq!(fpb.comparator(0), 0);

        // An invalid value from a session that crashed.
        fpb.memory.insert(FP_COMP0, 0x0000_0001);

        let second = N32H7xx::create_for_chip(&dual_core("N32H785xIx7"));
        second.normalize_secondary_fpb_if_due(&mut fpb, &secondary, false);
        assert_eq!(fpb.comparator(0), 0, "the next session must clear it");
    }

    /// The M4 AP of the vendor's dual-core layout.
    fn secondary_ap() -> FullyQualifiedApAddress {
        N32H7xx::create_for_chip(&dual_core("N32H785xIx7"))
            .secondary_ap
            .clone()
            .expect("the dual-core description has a second AP")
    }

    #[test]
    fn fp_num_code_reads_the_count_out_of_either_revision() {
        // Revision 0: the measured M4 value, six comparators.
        assert_eq!(N32H7xx::fpb_num_code(0x0000_0261), Some(6));
        // Revision 1: the M7 value, eight comparators.
        assert_eq!(N32H7xx::fpb_num_code(0x1000_0081), Some(8));
        assert_eq!(N32H7xx::fpb_num_code(0x2000_0081), None);
    }

    #[test]
    fn the_entry_breakpoint_takes_the_first_free_comparator() {
        let ap = secondary_ap();

        // Two user breakpoints in the first two comparators.
        let mut fpb = FpbMock::with_measured_m4_fpb(&[(0, 0x5508_04D9), (1, 0x9508_04ED)]);
        assert_eq!(
            N32H7xx::free_fpb_comparator(&mut fpb, &ap, 0x261).expect("a readable FPB"),
            Some(2)
        );

        // An invalid value is not free.
        fpb.memory.insert(fp_comp(2), 0x0000_0001);
        assert_eq!(
            N32H7xx::free_fpb_comparator(&mut fpb, &ap, 0x261).expect("a readable FPB"),
            Some(3)
        );

        for index in 0..6u32 {
            fpb.memory.insert(fp_comp(index), 0x1000_0001);
        }
        assert_eq!(
            N32H7xx::free_fpb_comparator(&mut fpb, &ap, 0x261).expect("a readable FPB"),
            None
        );
    }

    #[test]
    fn a_snapshot_puts_the_breakpoints_back_after_a_reset() {
        let ap = secondary_ap();
        let mut fpb = FpbMock::with_measured_m4_fpb(&[(0, 0x5508_04D9), (1, 0x9508_04ED)]);

        let snapshot = N32H7xx::snapshot_secondary_fpb(&mut fpb, &ap)
            .expect("a readable FPB")
            .expect("a revision-0 layout is walkable");

        // The entry breakpoint, set after the snapshot.
        fpb.memory.insert(fp_comp(2), 0x1508_0401);

        // The reset clears the comparators and ENABLE. The read-only fields
        // stay.
        fpb.memory.clear();
        fpb.memory.insert(FP_CTRL, 0x260);

        N32H7xx::restore_secondary_fpb(&mut fpb, &ap, &snapshot).expect("a writable FPB");

        assert_eq!(fpb.comparator(0), 0x5508_04D9);
        assert_eq!(fpb.comparator(1), 0x9508_04ED);
        assert_eq!(
            fpb.comparator(2),
            0,
            "the entry breakpoint must not come back"
        );
        assert_eq!(
            fpb.memory.get(&FP_CTRL),
            Some(&0x261),
            "the unit must be enabled again"
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
            "an unknown FPB revision must not be captured"
        );
    }
}
