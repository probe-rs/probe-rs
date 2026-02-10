//! Sequences for TMS570 devices
//!
//! The sequence used for catching a reset from Section 3 of the document
//! [JTAG Programmer Overview for Hercules-based Microcontrollers](https://www.ti.com/lit/an/spna230/spna230.pdf),
//! and largely involves creating a breakpoint, issuing some sort of reset,
//! then clearing memory using platform-specific register writes.
use super::icepick::{DefaultProtocol, Icepick};
use crate::MemoryMappedRegister;
use crate::architecture::arm::core::armv7a_debug_regs::Dbgdscr;
use crate::architecture::arm::core::armv7ar::{
    clear_hw_breakpoint, core_halted, get_hw_breakpoint, read_word_32, request_halt, run,
    set_hw_breakpoint, wait_for_core_halted, write_word_32,
};
use crate::architecture::arm::dp::{DebugPortError, DpAddress};
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::{ArmDebugSequence, ArmDebugSequenceError};
use crate::architecture::arm::{ArmError, DapProbe, Pins};
use crate::probe::WireProtocol;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// The TMS570 is at index 0 in the TAP chain
const TMS570_TAP_INDEX: u8 = 0;

/// How long to wait for memory to be cleared.
const ECC_RAM_CLEAR_TIMEOUT: Duration = Duration::from_millis(100);

/// Generic timeout to wait for a reset
const RESET_TIMEOUT: Duration = Duration::from_millis(100);

const MINITGCR: u32 = 0xffff_ff5c;
const MSINENA: u32 = 0xffff_ff60;
const MSTCGSTAT: u32 = 0xffff_ff68;

const SYSECR: u32 = 0xffff_ffe0;
const SYSECR_RESET: u32 = 1 << 15;

const SYSESR: u32 = 0xffff_ffe4;
/// These bits should be checked to look for a reset
const SYSESR_RESET_MASK: u32 = 0x0000_1747;
/// This value is present when a debug reset has completed
const SYSESR_RESET_VALUE: u32 = 0x0000_0000;

struct TemporaryCore<'a> {
    memory: &'a mut dyn ArmMemoryInterface,
    base_address: u64,
}

/// A temporary copy of the Armv7ar object that is used to manipulate the target
/// without maintaining a long-term view of the object.
impl<'a> TemporaryCore<'a> {
    pub fn new(memory: &'a mut dyn ArmMemoryInterface, base_address: u64) -> Self {
        TemporaryCore {
            memory,
            base_address,
        }
    }

    /// Resetting can disable ITR, which is needed to execute instructions. Ensure it's re-enabled.
    pub fn ensure_itren(&mut self) -> Result<(), ArmError> {
        let address = Dbgdscr::get_mmio_address_from_base(self.base_address)?;
        let mut dbgdscr = Dbgdscr(self.memory.read_word_32(address)?);
        if !dbgdscr.itren() {
            dbgdscr.set_itren(true);
            self.memory.write_word_32(address, dbgdscr.into())?;
        }
        Ok(())
    }

    pub fn write_word_32(&mut self, address: u32, data: u32) -> Result<(), ArmError> {
        self.halted_access(|core| {
            core.ensure_itren()?;
            write_word_32(core.memory, core.base_address, address, data)
        })
    }

    pub fn read_word_32(&mut self, address: u32) -> Result<u32, ArmError> {
        self.halted_access(|core| {
            core.ensure_itren()?;
            read_word_32(core.memory, core.base_address, address)
        })
    }

    pub fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), ArmError> {
        wait_for_core_halted(self.memory, self.base_address, timeout)
    }

    pub fn core_halted(&mut self) -> Result<bool, ArmError> {
        core_halted(self.memory, self.base_address)
    }

    pub fn request_halt(&mut self) -> Result<(), ArmError> {
        request_halt(self.memory, self.base_address)
    }

    pub fn run(&mut self) -> Result<(), ArmError> {
        run(self.memory, self.base_address)
    }

    pub fn halted_access<R>(
        &mut self,
        op: impl FnOnce(&mut Self) -> Result<R, ArmError>,
    ) -> Result<R, ArmError> {
        let was_halted = self.core_halted()?;
        if !was_halted {
            self.request_halt()?;
            self.wait_for_core_halted(Duration::from_millis(200))?;
        }
        let result = op(self);
        if !was_halted {
            self.run()?;
        }
        result
    }

    pub(crate) fn clear_hw_breakpoint(&mut self, bp_unit_index: usize) -> Result<(), ArmError> {
        clear_hw_breakpoint(self.memory, self.base_address, bp_unit_index)
    }

    pub(crate) fn set_hw_breakpoint(
        &mut self,
        bp_unit_index: usize,
        addr: u32,
    ) -> Result<(), ArmError> {
        set_hw_breakpoint(self.memory, self.base_address, bp_unit_index, addr)
    }

    pub(crate) fn get_hw_breakpoint(
        &mut self,
        bp_unit_index: usize,
    ) -> Result<Option<u32>, ArmError> {
        get_hw_breakpoint(self.memory, self.base_address, bp_unit_index)
    }
}

/// Handle reset and sequencing for TMS570 parts.
#[derive(Debug)]
pub struct TMS570 {
    /// A breakpoint that we will need to restore after catching a reset
    existing_breakpoint: AtomicU32,

    /// `true` if `existing_breakpoint` contains a valid breakpoint
    breakpoint_active: AtomicBool,

    /// `true` if the user has requested that we catch reset
    reset_catch: AtomicBool,
}

impl TMS570 {
    /// Create the sequencer for the TMS570 family of parts.
    pub fn create(_name: String) -> Arc<Self> {
        Arc::new(Self {
            existing_breakpoint: AtomicU32::new(0),
            breakpoint_active: AtomicBool::new(false),
            reset_catch: AtomicBool::new(false),
        })
    }
}

fn ensure_ntrst(interface: &mut dyn DapProbe, nrst: bool) -> Result<(), ArmError> {
    let mut pin_mask = Pins(0);
    pin_mask.set_ntrst(true);
    pin_mask.set_nreset(nrst);

    let mut pin_value = Pins(0);
    pin_value.set_ntrst(true);
    pin_value.set_nreset(true);

    let _ = interface.swj_pins(pin_value.0.into(), pin_mask.0.into(), 0)?;
    Ok(())
}

/// TMS570 has ECC RAM. Ensure it's cleared to avoid cascading failures. Without
/// this, writes to SRAM will trap, preventing execution from RAM.
fn clear_ecc_memory(core: &mut TemporaryCore) -> Result<(), ArmError> {
    core.write_word_32(MINITGCR, 0xa)?;
    core.write_word_32(MSINENA, 1)?;
    let start = Instant::now();
    loop {
        let mstcgstat = core.read_word_32(MSTCGSTAT)?;
        if mstcgstat & (1 << 8) != 0 {
            break;
        }
        if start.elapsed() >= ECC_RAM_CLEAR_TIMEOUT {
            tracing::error!(
                "Memory didn't clear after {} ms",
                ECC_RAM_CLEAR_TIMEOUT.as_millis()
            );
            return Err(ArmError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    tracing::trace!("Memory cleared after {} ms", start.elapsed().as_millis());
    core.write_word_32(MINITGCR, 0x5)?;
    Ok(())
}

impl ArmDebugSequence for TMS570 {
    fn reset_hardware_assert(&self, interface: &mut dyn DapProbe) -> Result<(), ArmError> {
        // Only toggle nRST. This is because the ICEPICK is completely nonresponsive
        // under nRST. It does, however, reset the system. Note that this will not
        // succeed, but the next time `probe-rs` is run the target will have been
        // reset.
        ensure_ntrst(interface, false)?;
        ensure_ntrst(interface, true)?;
        let mut icepick = Icepick::new(interface, DefaultProtocol::Jtag)?;
        icepick.select_tap(TMS570_TAP_INDEX, "TMS570")?;
        Err(ArmError::DebugPort(DebugPortError::Unsupported(
            "Hardware reset is not supported by TMS570".to_owned(),
        )))
    }

    fn reset_catch_set(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let base_address = debug_base.ok_or(ArmError::NoArmTarget)?;

        TemporaryCore::new(memory, base_address).halted_access(|core| {
            // If there is an existing breakpoint at address 0, note that down before
            // replacing it with our own breakpoint.
            let existing = core.get_hw_breakpoint(0)?;
            if let Some(existing) = existing {
                self.existing_breakpoint.store(existing, Ordering::Release);
            }
            self.breakpoint_active
                .store(existing.is_some(), Ordering::Relaxed);

            // Insert a breakpoint at address 0 to be hit when the core resets
            core.set_hw_breakpoint(0, 0)?;

            self.reset_catch.store(true, Ordering::Release);
            Ok(())
        })
    }

    fn reset_catch_clear(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        self.reset_catch.store(false, Ordering::Release);
        let base_address = debug_base.ok_or(ArmError::NoArmTarget)?;

        let mut core = TemporaryCore::new(memory, base_address);
        core.halted_access(|core| {
            if self.breakpoint_active.swap(false, Ordering::Release) {
                core.set_hw_breakpoint(0, self.existing_breakpoint.load(Ordering::Relaxed))?;
            } else {
                core.clear_hw_breakpoint(0)?;
            }

            clear_ecc_memory(core)?;

            Ok(())
        })
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let base_address = debug_base.ok_or(ArmError::NoArmTarget)?;

        let mut core = TemporaryCore::new(interface, base_address);

        core.request_halt()?;
        core.wait_for_core_halted(RESET_TIMEOUT)?;

        if self.reset_catch.load(Ordering::Relaxed) {
            core.write_word_32(SYSECR, SYSECR_RESET)?;

            // Wait for SYSESR to get to a sane value
            let start = Instant::now();
            loop {
                if start.elapsed() > RESET_TIMEOUT {
                    return Err(ArmError::Timeout);
                }
                let sysesr = core.read_word_32(SYSESR)?;
                if sysesr & SYSESR_RESET_MASK == SYSESR_RESET_VALUE {
                    break;
                }
            }
        } else {
            // If we're not catching reset, then this write will likely fail
            // (since the core will immediately start running).
            if let Err(e) = core.write_word_32(SYSECR, SYSECR_RESET) {
                tracing::info!("Caught error when triggering reset: {e}");
            }
        }
        Ok(())
    }

    fn debug_port_setup(
        &self,
        interface: &mut dyn DapProbe,
        _dp: DpAddress,
    ) -> Result<(), ArmError> {
        ensure_ntrst(interface, true)?;

        match interface.active_protocol() {
            Some(WireProtocol::Jtag) => {
                let mut icepick = Icepick::new(interface, DefaultProtocol::Jtag)?;
                icepick.select_tap(TMS570_TAP_INDEX, "TMS570")?;

                // Call the configure JTAG function. We don't derive the scan chain at runtime
                // for these devices, but regardless the scan chain must be told to the debug probe
                // We avoid the live scan for the following reasons:
                // 1. Only the ICEPICK is connected at boot so we need to manually the CPU to the scan chain
                // 2. Entering test logic reset disconnects the CPU again
                interface.configure_jtag(true)?;
            }
            Some(WireProtocol::Swd) => {
                return Err(ArmDebugSequenceError::SequenceSpecific(
                    "The tms570 family doesn't support SWD".into(),
                )
                .into());
            }
            _ => {
                return Err(ArmDebugSequenceError::SequenceSpecific(
                    "Cannot detect current protocol".into(),
                )
                .into());
            }
        }

        Ok(())
    }
}
