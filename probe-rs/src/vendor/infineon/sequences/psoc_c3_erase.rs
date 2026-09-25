//! Chip erase for dual-bank PSOC C3 devices, driven through the boot ROM SROM flash API.
//!
//! Whether a part is single- or dual-bank is a provisioning choice rather than a property of
//! the part number, and both report the same silicon ID. The target definition therefore
//! describes both layouts at once, with the bank-1 windows marked as aliases so that flash
//! operations stay away from addresses that do not exist on a single-bank part. The side
//! effect is that a chip erase silently leaves bank 1 programmed on a dual-bank part.
//!
//! Marking them as aliases is a static stand-in for a runtime property: the flash algorithm
//! selects its bank geometry from `FLASHC_FLASH_CTL` and fails `init` for a bank-1 address on
//! a single-bank part, and there is no hook to drop a region once the mode is known.
//!
//! This module closes that gap without touching the shared flash code: a dual-bank part is
//! erased here, a single-bank part offers no erase sequence and keeps using the flash
//! algorithms.
//!
//! The boot ROM has no chip- or bank-erase entry point, only `cyboot_flash_erase_row` for one
//! 512-byte row. Each call is made by parking the CM33 on that function with its arguments
//! in core registers and `lr` pointing at a breakpoint in RAM, so the core halts itself
//! when the call returns.

use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use probe_rs_target::{Chip, MemoryRegion};

use crate::{
    MemoryMappedRegister, RegisterId,
    architecture::arm::{
        ArmDebugInterface, ArmError, FullyQualifiedApAddress,
        core::{armv7m::Dhcsr, cortex_m},
        memory::ArmMemoryInterface,
        sequences::DebugEraseSequence,
    },
};

use super::psoc_c3_common::{self, FlashBankMode, SysApCsw};

/// Address holding the `cyboot_flash_erase_row` function pointer (spec Table 2).
const SROMAPI_ERASE_ROW: u64 = 0x1080_FFE0;

/// Status the SROM flash APIs return in `r0` when the operation succeeded.
const CYBOOT_SUCCESS: u32 = 0x0D50_B002;

/// Flash row size. Nothing smaller can be erased.
const ROW_SIZE: u64 = 512;

/// Base of the secure SRAM alias, used as scratch while driving the SROM API.
///
/// This follows the layout from the programming spec, which overlaps the window the flash
/// algorithm is loaded into. That is safe because an erase sequence completes before
/// `Flasher::load` uploads the algorithm, and that upload verifies what it wrote and then
/// resets the core. If that ordering ever changes, this scratch has to move.
const SRAM_S_BASE: u64 = 0x3400_0000;
/// Where the zeroed `flash_context_t` is placed (spec: `PROGRAM_CTX_OFFSET`).
const CTX_ADDR: u64 = SRAM_S_BASE;
/// Bytes zeroed for the flash context; the spec places the breakpoint immediately above it.
const CTX_LEN: u64 = 0x100;
/// Where the breakpoint the SROM call returns to is placed (spec: `PROGRAM_BKPT_OFFSET`).
const BKPT_ADDR: u64 = CTX_ADDR + CTX_LEN;
/// Two Thumb `BKPT #0` instructions (spec: `DUAL_BKPT_INSTR`).
const DUAL_BKPT_INSTR: u32 = 0xBE00_BE00;
/// Stack pointer handed to the SROM call (spec: `SRAM_S_BASE + 0x1000`).
const STACK_TOP: u64 = SRAM_S_BASE + 0x1000;

// DCRSR REGSEL values (spec "Redefine some REGSEL values").
const REGSEL_R0: u16 = 0x00;
const REGSEL_R1: u16 = 0x01;
const REGSEL_LR: u16 = 0x0E;
const REGSEL_PC: u16 = 0x0F;
const REGSEL_MSP: u16 = 0x11;
/// Packs `CONTROL`/`FAULTMASK`/`BASEPRI`/`PRIMASK` into one word.
const REGSEL_PRIMASK: u16 = 0x14;
const REGSEL_MSPLIM_S: u16 = 0x1C;

/// `PRIMASK` set, the rest cleared: interrupts masked, core privileged and on `MSP`.
const IRQS_MASKED: u32 = 0x0000_0001;

/// How long a single row erase may take before it is treated as hung.
///
/// A row takes about 15ms in practice, but the target definitions declare an
/// `erase_sector_timeout` of 5s (E/F parts) to 15s (G parts) for the same 512-byte sector,
/// so anything tighter risks calling a slow row a failure. The timeout only costs time when
/// an erase really is stuck.
const ROW_TIMEOUT: Duration = Duration::from_secs(15);
/// How long to wait for the core to halt when preparing for the erase.
const HALT_TIMEOUT: Duration = Duration::from_millis(500);
/// How long the boot ROM needs after a soft reset before the core can be halted again.
/// Sized for the slowest family, since one erase sequence serves them all.
const RESET_DELAY: Duration = Duration::from_millis(400);

/// Secure S-bus base of flash bank 0.
const BANK0_S: u64 = 0x3200_0000;
/// Secure S-bus base of flash bank 1, which only exists in dual-bank mode.
const BANK1_S: u64 = 0x3280_0000;

/// The spans a chip erase has to cover on a dual-bank part.
///
/// Dual-bank mode halves the main flash and maps the upper half at [`BANK1_S`], so both banks
/// are the size of the declared bank-1 window; the definition still lists bank 0 at its full
/// single-bank size. `None` means there is no bank-1 window, so the part cannot be dual-bank.
fn dual_bank_layout(chip: &Chip) -> Option<Vec<Range<u64>>> {
    let bank1 = chip
        .memory_map
        .iter()
        .filter_map(MemoryRegion::as_nvm_region)
        .find(|region| region.range.start == BANK1_S)?;

    let bank_len = bank1.range.end - bank1.range.start;

    Some(vec![
        BANK0_S..BANK0_S + bank_len,
        BANK1_S..BANK1_S + bank_len,
    ])
}

/// Decides how a PSOC C3 has to be erased, shared by the debug sequences of all families.
///
/// The layout can only be learned from the live device, while the geometry has to come from
/// the chip definition up front, because [`DebugEraseSequence`] only ever sees a debug
/// interface.
#[derive(Debug)]
pub(super) struct DualBankErase {
    dual_bank: AtomicBool,
    banks: Option<Vec<Range<u64>>>,
}

impl DualBankErase {
    pub(super) fn new(chip: &Chip) -> Self {
        Self {
            dual_bank: AtomicBool::new(false),
            banks: dual_bank_layout(chip),
        }
    }

    /// Record what the device reported during attach.
    pub(super) fn set_mode(&self, mode: FlashBankMode) {
        self.dual_bank
            .store(mode == FlashBankMode::Dual, Ordering::Relaxed);
    }

    /// The erase sequence to use, or `None` to keep the stock flash-algorithm path.
    pub(super) fn sequence(
        &self,
        cm33_ap: FullyQualifiedApAddress,
    ) -> Option<Arc<dyn DebugEraseSequence>> {
        if !self.dual_bank.load(Ordering::Relaxed) {
            return None;
        }

        Some(Arc::new(PsocC3ChipErase {
            cm33_ap,
            banks: self.banks.clone()?,
        }))
    }
}

/// Erases every flash bank of a dual-bank PSOC C3 through the SROM row-erase API.
#[derive(Debug)]
struct PsocC3ChipErase {
    /// The CM33 access port the SROM calls are driven through.
    cm33_ap: FullyQualifiedApAddress,
    /// The banks to erase, in the secure S-bus alias.
    banks: Vec<Range<u64>>,
}

/// Halt the core and wait until it reports being halted.
fn halt(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    let mut dhcsr = Dhcsr(0);
    dhcsr.set_c_debugen(true);
    dhcsr.set_c_halt(true);
    dhcsr.enable_write();
    memory.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

    wait_for_halt(memory, HALT_TIMEOUT, "the core did not halt")
}

/// Let the core run. Used to enter the SROM function; it halts itself again on the breakpoint.
fn resume(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    let mut dhcsr = Dhcsr(0);
    dhcsr.set_c_debugen(true);
    dhcsr.set_c_halt(false);
    dhcsr.enable_write();
    memory.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
    memory.flush()
}

/// Poll `DHCSR.S_HALT` until the core is halted or `timeout` elapses.
///
/// A timeout reports `DHCSR` rather than a bare [`ArmError::Timeout`], because that is what
/// explains the stall: `S_LOCKUP` means the core faulted, `S_SLEEP` that it never started.
/// probe-rs is usually run without tracing, so the error text is the only place it shows up.
fn wait_for_halt(
    memory: &mut dyn ArmMemoryInterface,
    timeout: Duration,
    what: &str,
) -> Result<(), ArmError> {
    let deadline = Instant::now() + timeout;
    loop {
        let dhcsr = Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?);
        if dhcsr.s_halt() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ArmError::Other(format!(
                "PSOC C3: {what} within {timeout:?} (DHCSR={:#010x}, S_LOCKUP={}, S_SLEEP={})",
                dhcsr.0,
                dhcsr.s_lockup() as u8,
                dhcsr.s_sleep() as u8,
            )));
        }
    }
}

impl PsocC3ChipErase {
    /// Reset the core if it is locked up, so that it can run the SROM call at all.
    ///
    /// The API is invoked by resuming the core, and a locked-up core does not resume, so
    /// every row would time out. Only a reset clears lockup, and finding the device in that
    /// state is normal: a crashed application is what a chip erase is used to recover from.
    ///
    /// Must run before the core is halted, because halting clears `S_LOCKUP` while leaving
    /// the core in the faulted state, making the condition undetectable afterwards.
    fn recover_from_lockup(&self, memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        let dhcsr = Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?);
        if !dhcsr.s_lockup() {
            return Ok(());
        }

        tracing::debug!("PSOC C3: the core is locked up, resetting it before the erase");
        psoc_c3_common::halt_and_soft_reset(memory, RESET_DELAY)?;

        let dp = memory.fully_qualified_address().dp();
        let arm = memory.get_arm_debug_interface()?;

        // The reset latches the DP sticky-error flags, and every subsequent AP access
        // faults until they are cleared.
        psoc_c3_common::clear_dp_sticky_errors(arm, dp);

        // The soft reset clears the CM33 AP CSW, so it has to be re-applied before the next
        // AP access.
        psoc_c3_common::apply_standard_cm33_csw(arm, &self.cm33_ap)?;

        Ok(())
    }

    /// Read the `cyboot_flash_erase_row` pointer and lay out the scratch the call needs.
    ///
    /// Both live in secure aliases, so these accesses go through the SYS AP as the reference
    /// flow in the programming spec does: it always drives secure transfers, whereas the
    /// CM33 AP only does so while the part reports secure debug as enabled.
    fn prepare_scratch(&self, interface: &mut dyn ArmDebugInterface) -> Result<u32, ArmError> {
        let sys_ap = psoc_c3_common::sys_ap(self.cm33_ap.dp());
        interface.write_raw_ap_register(
            &sys_ap,
            psoc_c3_common::AP_CSW,
            SysApCsw::secure_word().0,
        )?;

        let erase_row_fn =
            psoc_c3_common::read_mem32(interface, &sys_ap, SROMAPI_ERASE_ROW as u32)?;
        tracing::debug!("PSOC C3: cyboot_flash_erase_row is at {erase_row_fn:#010x}");

        // Jumping to a pointer that was never read back would run the core into the weeds,
        // where it can only be recovered by a reset. Report the bad read instead.
        if erase_row_fn == 0 || erase_row_fn == u32::MAX {
            return Err(ArmError::Other(format!(
                "PSOC C3: the SROM flash API entry at {SROMAPI_ERASE_ROW:#010x} read back as \
                 {erase_row_fn:#010x}, so the erase call cannot be made"
            )));
        }

        // A blocking call needs an all-zero flash context.
        for offset in (0..CTX_LEN).step_by(4) {
            psoc_c3_common::write_mem32(interface, &sys_ap, (CTX_ADDR + offset) as u32, 0)?;
        }

        // The SROM call returns to `lr`, which points here, so the core halts itself.
        psoc_c3_common::write_mem32(interface, &sys_ap, BKPT_ADDR as u32, DUAL_BKPT_INSTR)?;

        Ok(erase_row_fn)
    }

    /// Walk every bank one row at a time, calling the SROM API for each.
    fn erase_rows(
        &self,
        interface: &mut dyn ArmDebugInterface,
        erase_row_fn: u32,
        total_rows: u64,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(&self.cm33_ap)?;

        // Clear any stack limit left by the application, which would otherwise fault the call.
        cortex_m::write_core_reg(&mut *memory, RegisterId(REGSEL_MSPLIM_S), 0)?;

        // Row 0 holds the vector table, so once it is erased an interrupt arriving while the
        // SROM call runs would vector through blank flash and lock the core up.
        cortex_m::write_core_reg(&mut *memory, RegisterId(REGSEL_PRIMASK), IRQS_MASKED)?;

        let mut erased_rows = 0u64;
        for bank in &self.banks {
            tracing::debug!("PSOC C3: erasing {:#010x}..{:#010x}", bank.start, bank.end);

            for address in bank.clone().step_by(ROW_SIZE as usize) {
                erase_row(&mut *memory, erase_row_fn, address)?;

                erased_rows += 1;
                if erased_rows.is_multiple_of(128) {
                    tracing::debug!("PSOC C3: {erased_rows}/{total_rows} rows erased");
                }
            }
        }

        Ok(())
    }
}

impl DebugEraseSequence for PsocC3ChipErase {
    fn erase_all(&self, interface: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        let total_rows: u64 = self
            .banks
            .iter()
            .map(|b| (b.end - b.start) / ROW_SIZE)
            .sum();
        let started = Instant::now();
        tracing::info!(
            "PSOC C3: erasing {} bank(s), {total_rows} rows, via the SROM flash API",
            self.banks.len()
        );

        // The SROM call runs on the CM33, so the core has to be halted before its registers
        // can be repurposed.
        let mut memory = interface.memory_interface(&self.cm33_ap)?;
        self.recover_from_lockup(&mut *memory)?;
        halt(&mut *memory)?;
        drop(memory);

        let erase_row_fn = self.prepare_scratch(interface)?;
        let result = self.erase_rows(interface, erase_row_fn, total_rows);

        if result.is_err() {
            // A call that never returned leaves the core running on the scratch stack with a
            // synthetic `lr`. Left like that it takes the rest of the session down with it,
            // so put it back under debugger control before reporting the failure.
            if let Err(e) = interface
                .memory_interface(&self.cm33_ap)
                .and_then(|mut memory| halt(&mut *memory))
            {
                tracing::warn!("PSOC C3: could not halt the core after the failed erase: {e}");
            }
        }

        result?;

        tracing::info!(
            "PSOC C3: erased {total_rows} rows in {:.1}s",
            started.elapsed().as_secs_f32()
        );

        Ok(())
    }
}

/// Erase one row by calling `cyboot_flash_erase_row` on the target.
///
/// The Thumb bit has to be set on `lr`, otherwise returning from the call raises a usage
/// fault.
fn erase_row(
    memory: &mut dyn ArmMemoryInterface,
    erase_row_fn: u32,
    address: u64,
) -> Result<(), ArmError> {
    cortex_m::write_core_reg(memory, RegisterId(REGSEL_R0), address as u32)?;
    cortex_m::write_core_reg(memory, RegisterId(REGSEL_R1), CTX_ADDR as u32)?;
    cortex_m::write_core_reg(memory, RegisterId(REGSEL_MSP), STACK_TOP as u32)?;
    cortex_m::write_core_reg(memory, RegisterId(REGSEL_LR), BKPT_ADDR as u32 | 1)?;
    cortex_m::write_core_reg(memory, RegisterId(REGSEL_PC), erase_row_fn)?;

    resume(memory)?;
    wait_for_halt(
        memory,
        ROW_TIMEOUT,
        &format!("the SROM erase of row {address:#010x} did not return"),
    )?;

    // Without this check a rejected erase would look like a successful one.
    let status = cortex_m::read_core_reg(memory, RegisterId(REGSEL_R0))?;
    if status != CYBOOT_SUCCESS {
        return Err(ArmError::Other(format!(
            "PSOC C3: erasing flash row {address:#010x} failed, \
             cyboot_flash_erase_row returned {status:#010x}"
        )));
    }

    Ok(())
}
