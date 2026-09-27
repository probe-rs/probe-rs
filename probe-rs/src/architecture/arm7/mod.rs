//! ARM7TDMI architecture support
//!
//! Debugging goes through EmbeddedICE (see [`communication_interface`]). Hardware behaviour
//! the implementation relies on, verified on the MC1322x:
//!
//! - While halted, every chain-1 register capture clocks injected instructions through the
//!   core and advances the pipeline by a few instructions. The halt PC is therefore captured
//!   once at debug entry and cached (`Arm7tdmiCommunicationInterface::cached_halt_pc`); PC reads,
//!   halt reasons and `resume()` all use that value instead of a fresh capture.
//! - The core is converted to ARM state at every halt, since all injected sequences are
//!   ARM-encoded. Whether to resume in Thumb is remembered in software and also stored on the
//!   target (the halt record), so a later session can take over the halt.
//! - Halts use a wildcard fetch watchpoint rather than DBGRQ where possible, because a DBGRQ
//!   halt during a pipeline refill reports the wrong PC.
//! - Memory accesses run at system speed through injected `LDM`/`STM` instructions, which
//!   clobber R0/R1 (R0-R4 for bulk reads); these are saved and restored around each access.

use crate::{
    Architecture, CoreInformation, CoreInterface, CoreRegister, CoreStatus, CoreType, Error,
    HaltReason, InstructionSet, MemoryInterface,
    core::{BreakpointCause, CoreRegisters, RegisterId, RegisterValue, VectorCatchCondition},
    memory::{MemoryNotAlignedError, valid_32bit_address},
    semihosting::{SemihostingCommand, check_for_semihosting},
};
use std::sync::Arc;
use std::time::Duration;

pub mod communication_interface;
pub mod registers;
pub mod sequences;
mod step_sim;

use communication_interface::CPSR_TBIT;
pub use communication_interface::{
    Arm7tdmiCommunicationInterface, Arm7tdmiDebugInterfaceState, Arm7tdmiError,
};
use registers::*;
pub use sequences::{Arm7tdmiDebugSequence, DefaultArm7tdmiSequence};

/// The hardware unit reserved for `VectorCatchCondition::Svc` (see
/// [`Arm7tdmi::enable_vector_catch`]) while it is enabled.
///
/// EmbeddedICE has no vector-catch register (no DBGVCR equivalent), so one of the
/// [`Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT`] units breaks on the SVC vector
/// instead. It is tracked in `Arm7tdmiState::hw_breakpoints` like an ordinary breakpoint, so
/// `Core::set_hw_breakpoint`'s free-unit search avoids it.
const SVC_VECTOR_UNIT: usize = 0;

/// Address of the SVC exception vector (ARM7TDMI has no vector relocation).
const SVC_VECTOR_ADDRESS: u32 = 0x08;

/// ARM7TDMI core state
#[derive(Debug)]
pub struct Arm7tdmiState {
    initialized: bool,
    current_state: CoreStatus,
    breakpoints_enabled: bool,
    hw_breakpoints: [Option<u64>; Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT],
    /// Data watchpoints, by unit. A unit holds either a breakpoint (`hw_breakpoints`) or a data
    /// watchpoint (here), never both.
    data_watchpoints: [Option<u64>; Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT],
    /// Whether `enable_vector_catch(VectorCatchCondition::Svc)` is currently active.
    svc_vector_catch_enabled: bool,
    /// The semihosting command decoded at the current SVC-vector-catch halt, if any. Cached so
    /// repeated `status()` polls don't re-decode it; consumed by `run()` to return from the call.
    semihosting_command: Option<SemihostingCommand>,
    /// Whether `status()` already checked the current halt at the SVC vector for a semihosting
    /// call, so repeated polls don't redo the register/memory reads (or repeat a warning).
    /// Cleared whenever the core runs, steps or resets.
    svc_halt_checked: bool,
}

impl Arm7tdmiState {
    /// Create a new ARM7TDMI core state
    pub fn new() -> Self {
        Self {
            initialized: false,
            current_state: CoreStatus::Unknown,
            breakpoints_enabled: false,
            hw_breakpoints: [None; Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT],
            data_watchpoints: [None; Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT],
            svc_vector_catch_enabled: false,
            semihosting_command: None,
            svc_halt_checked: false,
        }
    }
}

impl Default for Arm7tdmiState {
    fn default() -> Self {
        Self::new()
    }
}

/// ARM7TDMI core implementation
pub struct Arm7tdmi<'probe> {
    interface: Arm7tdmiCommunicationInterface<'probe>,
    state: &'probe mut Arm7tdmiState,
    sequence: Arc<dyn Arm7tdmiDebugSequence>,
}

/// Make a halted ARMv4T core resume in ARM state, by clearing CPSR.T if it is set.
///
/// A PC write keeps the instruction set the core was halted in, so callers that redirect PC
/// into ARM code from a halt that may have been in Thumb state (the flash loader, the RAM-boot
/// redirect) call this first.
pub(crate) fn enter_arm_state(core: &mut crate::Core<'_>) -> Result<(), Error> {
    let cpsr: u32 = core.read_core_reg(CPSR.id)?;
    if cpsr & CPSR_TBIT != 0 {
        core.write_core_reg(CPSR.id, cpsr & !CPSR_TBIT)?;
    }
    Ok(())
}

impl<'probe> Arm7tdmi<'probe> {
    /// Execute exactly one instruction (see [`CoreInterface::step`]) and return the PC of the
    /// next one. Leaves the hardware units configured for stepping - the caller restores them.
    fn single_step(
        &mut self,
        current_pc: u32,
        cpsr: u32,
        next: step_sim::NextInstruction,
    ) -> Result<u32, Error> {
        self.interface
            .configure_step_watchpoints(current_pc, next.pc)?;
        // Interrupts stay masked while stepping, so the step executes the instruction instead
        // of entering a pending IRQ/FIQ handler.
        self.interface.resume_for_step()?;

        // DBGACK+SYSCOMP, like OpenOCD's `arm7_9_step`: DBGACK alone can assert before the core
        // reached the step watchpoint.
        let start = std::time::Instant::now();
        while !self.interface.is_halted_with_syscomp()? {
            if start.elapsed() >= Duration::from_secs(1) {
                // Don't leave the core running behind the caller's back: stop it the regular
                // way, so the core state stays consistent, and report the failed step.
                tracing::warn!("step at {current_pc:#010x} did not complete, halting the core");
                // Both units hold the step configuration, which `step()` restores afterwards.
                self.interface.halt_precisely_with_unit(0)?;
                self.state.current_state = CoreStatus::Halted(HaltReason::Request);
                return Err(Error::Timeout);
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        // Not durable on its own, see `latch_current_halt`.
        self.interface.latch_current_halt()?;

        // The instruction set the core really halted in (a BX, an exception entry or an
        // exception return can change it). Every chain-1 sequence assumes ARM state, so a
        // Thumb halt is converted, using the predicted PC instead of a fresh capture.
        let thumb = self.interface.halted_in_thumb()?;
        let mut pc = next.pc;
        // Where the core is if a predicted exception wasn't taken (an encoding the simulator
        // considers undefined that this silicon executes after all).
        let sequential = current_pc.wrapping_add(if cpsr & CPSR_TBIT != 0 { 2 } else { 4 });
        if thumb {
            // An exception entry always switches to ARM state.
            if let Some(exception) = next.exception {
                tracing::warn!(
                    "step at {current_pc:#010x}: predicted {exception:?}, but the core is still \
                     in Thumb state; assuming it executed the instruction"
                );
                pc = sequential;
            }
            self.interface.convert_to_arm_after_thumb_step(pc)?;
        } else if let Some(exception) = next.exception {
            let mode_after = self.interface.read_core_register(16)? & 0x1F;
            if mode_after != exception.mode() {
                tracing::warn!(
                    "step at {current_pc:#010x}: predicted {exception:?}, but the core is in \
                     mode {mode_after:#04x}; assuming it executed the instruction"
                );
                pc = sequential;
            }
        } else if !next.changes_mode {
            // An exception the instruction itself doesn't imply (IRQ/FIQ are masked, so in
            // practice a data abort, or an undefined instruction the simulator doesn't know):
            // the core entered the exception's mode and halted at its vector instead.
            let mode_before = cpsr & 0x1F;
            let mode_after = self.interface.read_core_register(16)? & 0x1F;
            if mode_after != mode_before
                && let Some(exception) = step_sim::Exception::from_mode(mode_after)
            {
                tracing::debug!("step at {current_pc:#010x} entered {exception:?}");
                pc = exception.vector();
            }
        }
        if thumb != next.thumb && pc == next.pc {
            tracing::warn!(
                "step at {current_pc:#010x}: predicted {} state, core halted in {} state",
                if next.thumb { "Thumb" } else { "ARM" },
                if thumb { "Thumb" } else { "ARM" },
            );
        }

        // The step's own `resume()` consumed the cached resume PC and Thumb flag; record where
        // the core is now, the same way every other halt path does.
        self.interface.cache_resume_pc(pc);
        self.interface.set_pending_resume_thumb(thumb);
        self.interface.mark_debug_entry_done()?;
        Ok(pc)
    }

    /// Halt the core with an exact halt PC, through a wildcard fetch watchpoint (see
    /// [`Arm7tdmiCommunicationInterface::halt_at_next_instruction`]). Uses a free unit if there
    /// is one; otherwise borrows one, preferring the unit that doesn't hold the SVC vector catch
    /// (a semihosting call in that short window would be missed). Falls back to a DBGRQ halt if
    /// the watchpoint doesn't fire.
    fn halt_precisely(&mut self) -> Result<(), Error> {
        let free_unit = self
            .state
            .hw_breakpoints
            .iter()
            .zip(&self.state.data_watchpoints)
            .position(|(breakpoint, watchpoint)| breakpoint.is_none() && watchpoint.is_none());
        let unit = free_unit.unwrap_or(if self.state.svc_vector_catch_enabled {
            1 - SVC_VECTOR_UNIT
        } else {
            0
        });
        self.interface.halt_precisely_with_unit(unit)?;
        Ok(())
    }

    /// Why the core halted on its own (not through `halt()`/`step()`), inferred from what is
    /// armed: EmbeddedICE doesn't report a reason. A halt PC on an armed breakpoint is that
    /// breakpoint (or, at the SVC vector with the vector catch on, that exception); otherwise an
    /// armed data watchpoint is the only thing that can have stopped the core.
    fn fresh_halt_reason(&self) -> HaltReason {
        // A halt this debugger requested (e.g. `ensure_halted` re-halting a core that didn't
        // stay halted) isn't a breakpoint or watchpoint hit.
        if self.interface.halt_was_requested() {
            return HaltReason::Request;
        }
        let Some(pc) = self.interface.cached_halt_pc().map(u64::from) else {
            return HaltReason::Unknown;
        };
        if self.state.svc_vector_catch_enabled && pc == SVC_VECTOR_ADDRESS as u64 {
            HaltReason::Exception
        } else if self.state.hw_breakpoints.contains(&Some(pc)) {
            HaltReason::Breakpoint(BreakpointCause::Hardware)
        } else if self.state.data_watchpoints.iter().any(Option::is_some) {
            HaltReason::Watchpoint
        } else {
            HaltReason::Unknown
        }
    }

    /// Reprogram both hardware units from the given bookkeeping (after `step()` used them),
    /// attempting every unit even if one fails.
    fn restore_hw_units(
        &mut self,
        breakpoints: [Option<u64>; Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT],
        watchpoints: [Option<u64>; Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT],
    ) -> Result<(), Error> {
        let mut result = Ok(());
        for (unit, (breakpoint, watchpoint)) in breakpoints.into_iter().zip(watchpoints).enumerate()
        {
            let restored = match (breakpoint, watchpoint) {
                (Some(addr), _) => self.interface.set_hw_breakpoint(unit, addr as u32),
                (None, Some(addr)) => self.interface.set_hw_data_watchpoint(unit, addr as u32),
                (None, None) => self.interface.clear_hw_breakpoint(unit),
            };
            if result.is_ok() {
                result = restored.map_err(Error::from);
            }
        }
        result
    }

    /// Create a new ARM7TDMI core
    pub fn new(
        interface: Arm7tdmiCommunicationInterface<'probe>,
        state: &'probe mut Arm7tdmiState,
        sequence: Arc<dyn Arm7tdmiDebugSequence>,
    ) -> Result<Self, Error> {
        let mut core = Self {
            interface,
            state,
            sequence,
        };

        if !core.state.initialized {
            core.state.initialized = true;
            // One-time TAP reset and unit clearing, see `Arm7tdmiCommunicationInterface::new`.
            core.interface.init()?;
            core.sequence.debug_core_start(&mut core.interface)?;
            // A core that is already halted was left that way by a previous session (killed,
            // or a tool that doesn't resume on exit). Take over its halt state if that session
            // recorded it: debug-state entry already converted a Thumb halt to ARM state, which
            // can't be undone or detected from the core itself. A core halted in Thumb state was
            // never converted, so a record found then is stale; the halt is entered normally.
            if core.interface.is_halted()? {
                if core.interface.halted_in_thumb()? {
                    core.interface.discard_halt_record()?;
                } else if !core.interface.adopt_recorded_halt()? {
                    tracing::warn!(
                        "The core was already halted in ARM state when attaching, without state \
                         left by a probe-rs session. If another debugger halted it in Thumb code, \
                         resuming it will misbehave; reset the target if it does."
                    );
                }
            }
            // Halt and cache the halt PC for a later bare `resume()` (e.g. the plain
            // `read`/`write` commands: attach, halt, access, resume).
            core.halt_precisely()?;
            // Record the halt, as `Core::halt()` does: otherwise the first `status()` sees a
            // "fresh" halt and re-runs debug-state entry (see `status()`).
            core.state.current_state = CoreStatus::Halted(HaltReason::Request);
        }

        Ok(core)
    }
}

impl<'probe> CoreInterface for Arm7tdmi<'probe> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            // Must go through `status()`, which latches a fresh watchpoint halt and converts
            // the core to ARM state (`latch_watchpoint_halt`); otherwise register reads after a
            // Thumb-mode halt would misdecode.
            if matches!(self.status()?, CoreStatus::Halted(_)) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        Err(Error::Timeout)
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        // Through `status()`, see `wait_for_core_halted`.
        Ok(matches!(self.status()?, CoreStatus::Halted(_)))
    }

    fn status(&mut self) -> Result<CoreStatus, Error> {
        let is_halted = self.interface.is_halted()?;

        if !is_halted {
            self.state.current_state = CoreStatus::Running;
            self.state.svc_halt_checked = false;
            return Ok(self.state.current_state);
        }

        // A fresh halt must be latched before any chain-1 access (see `latch_watchpoint_halt`),
        // also for external callers that only use `status()`. Skipped for halts `halt()` /
        // `step()` already latched.
        let fresh_halt = !matches!(self.state.current_state, CoreStatus::Halted(_));
        if fresh_halt {
            self.interface.latch_watchpoint_halt()?;
        }

        // EmbeddedICE reports no halt reason, so a fresh halt's reason is inferred from the
        // armed units (see `fresh_halt_reason`). A repeated poll keeps the known reason (e.g.
        // `Request`/`Step`) so pollers such as the DAP server see a consistent one; the
        // semihosting check below runs on every poll (see `Arm7tdmiState::semihosting_command`).
        let mut reason = if fresh_halt {
            self.fresh_halt_reason()
        } else if let CoreStatus::Halted(existing) = self.state.current_state {
            existing
        } else {
            HaltReason::Unknown
        };

        // The SVC vector catch is recognised by the halt PC being the SVC vector.
        if self.state.svc_vector_catch_enabled {
            // Use the cached halt PC (see `cached_halt_pc`).
            let pc: u32 = self
                .interface
                .cached_halt_pc()
                .ok_or_else(|| Error::Other("no cached halt PC available".to_string()))?;
            tracing::debug!(
                "SVC vector-catch halt check: pc={pc:#010x} (expected {SVC_VECTOR_ADDRESS:#010x})"
            );
            if pc == SVC_VECTOR_ADDRESS && !self.state.svc_halt_checked {
                self.state.svc_halt_checked = true;
                let lr: u32 = self.read_core_reg(LR.id)?.try_into()?;
                let cached = self.state.semihosting_command.take();
                // A request the target set up wrongly (e.g. an unaligned parameter block) must
                // not make every status poll fail: report the halt as the SVC exception.
                self.state.semihosting_command = match check_for_semihosting(cached, self, pc, lr) {
                    Ok(command) => command,
                    Err(e) => {
                        tracing::warn!("Could not decode the semihosting request: {e}");
                        reason = HaltReason::Exception;
                        None
                    }
                };
                if let Some(command) = self.state.semihosting_command {
                    reason = HaltReason::Breakpoint(BreakpointCause::Semihosting(command));
                }
            }
        }

        self.state.current_state = CoreStatus::Halted(reason);
        Ok(self.state.current_state)
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.halt_precisely()?;

        // Set before `wait_for_core_halted`: `status()` can't tell why the core halted, and it
        // must see this halt as already latched, or `latch_watchpoint_halt` would run again,
        // capturing PC once more and replacing the DBGRQ-corrected PC with a watchpoint-style
        // one.
        self.state.current_state = CoreStatus::Halted(HaltReason::Request);

        self.wait_for_core_halted(timeout)?;

        // The PC cached while latching the halt (see `cached_halt_pc`).
        let pc: u32 = self
            .interface
            .cached_halt_pc()
            .ok_or_else(|| Error::Other("no cached halt PC available".to_string()))?;

        Ok(CoreInformation { pc: pc as u64 })
    }

    fn run(&mut self) -> Result<(), Error> {
        self.state.svc_halt_checked = false;
        if self.state.semihosting_command.take().is_some() {
            // Halted at the SVC vector for a semihosting call: return to the caller as the SVC
            // would have.
            self.interface.return_from_exception()?;
        } else if self
            .interface
            .cached_halt_pc()
            .is_some_and(|pc| self.state.hw_breakpoints.contains(&Some(pc as u64)))
        {
            // Step over a hardware breakpoint at the halt PC first, or the core re-traps
            // immediately. Other backends always step first; here it is limited to breakpoint
            // halts, which stop at an exact instruction boundary, unlike a DBGRQ halt. `step()`
            // leaves the core in ARM state (see `convert_to_arm_after_thumb_step`).
            self.step()?;
            // Stepping over a breakpoint on an SVC lands on the SVC vector, where the vector
            // catch would have stopped a free run: stay halted so `status()` reports the call.
            if self.state.svc_vector_catch_enabled
                && self.interface.cached_halt_pc() == Some(SVC_VECTOR_ADDRESS)
            {
                // Reported as the SVC exception (refined to the semihosting call, if it is
                // one), not as the step that got here.
                self.state.current_state = CoreStatus::Halted(HaltReason::Exception);
                return Ok(());
            }
        }

        self.interface.resume()?;
        self.state.current_state = CoreStatus::Running;
        Ok(())
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.sequence.reset_system(&mut self.interface)?;
        // A stale command would make `run()` return from an exception the reset left.
        self.state.semihosting_command = None;
        self.state.svc_halt_checked = false;
        // A hardware reset clears the EmbeddedICE units, so drop the bookkeeping; otherwise
        // e.g. `enable_vector_catch`'s "already enabled" shortcut would never re-arm it.
        self.state.hw_breakpoints =
            [None; Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT];
        self.state.data_watchpoints =
            [None; Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT];
        self.state.svc_vector_catch_enabled = false;
        self.state.current_state = CoreStatus::Running;
        Ok(())
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.state.semihosting_command = None;
        self.state.svc_halt_checked = false;
        // The reset clears the units, see `reset`.
        self.state.hw_breakpoints =
            [None; Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT];
        self.state.data_watchpoints =
            [None; Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT];
        self.state.svc_vector_catch_enabled = false;
        self.sequence.reset_catch_set(&mut self.interface)?;
        self.sequence.reset_system(&mut self.interface)?;
        self.sequence.reset_catch_clear(&mut self.interface)?;

        // This halts wherever the core is once JTAG works again, not at the reset vector.
        // OpenOCD's method (DBGRQ while only nSRST is held) doesn't work on the MC1322x: its TAP
        // reads all zeros during nSRST and for a few milliseconds after, by which time the boot
        // ROM is running (verified with nTRST inactive).
        self.halt_precisely()?;
        // See `halt()`.
        self.state.current_state = CoreStatus::Halted(HaltReason::Request);
        self.wait_for_core_halted(timeout)?;

        let pc: u32 = self.read_core_reg(PC.id)?.try_into()?;
        // Avoid a later PC re-read in `resume()`, see `cache_resume_pc`.
        self.interface.cache_resume_pc(pc);

        Ok(CoreInformation { pc: pc as u64 })
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        // A step at a semihosting halt executes the SVC vector itself: the call is no longer
        // pending, so a later `run()` must not return from it.
        self.state.semihosting_command = None;
        self.state.svc_halt_checked = false;
        // Predict the next PC by simulating the current instruction, like OpenOCD's
        // `arm7_9_step`/`arm_simulate_step` (see `step_sim`), then run to it with both units as
        // a range-chained pair (see `configure_step_watchpoints`); a lone watchpoint one
        // instruction ahead doesn't fire reliably. The user's units are restored afterwards,
        // also on failure.
        let (current_pc, cpsr, next) = step_sim::calculate_next_pc(&mut self.interface)?;

        let breakpoints = self.state.hw_breakpoints;
        let watchpoints = self.state.data_watchpoints;
        let stepped = self.single_step(current_pc, cpsr, next);
        let restored = self.restore_hw_units(breakpoints, watchpoints);
        let pc = stepped?;
        restored?;

        self.state.current_state = CoreStatus::Halted(HaltReason::Step);

        // Report the predicted PC, where the step watchpoint stopped the core; re-reading it
        // would advance the pipeline.
        Ok(CoreInformation { pc: pc as u64 })
    }

    fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, Error> {
        let value = self.interface.read_core_register(address.0 as u8)?;
        Ok(RegisterValue::U32(value))
    }

    fn read_core_regs_batch(
        &mut self,
        addresses: &[RegisterId],
    ) -> Vec<Result<RegisterValue, Error>> {
        // Read R0-R15 with one `STMIA` (see `read_core_registers`); others go through
        // `read_core_reg`.
        let mask: u16 = addresses.iter().fold(0u16, |mask, address| {
            if address.0 <= 15 {
                mask | (1 << address.0)
            } else {
                mask
            }
        });

        let mut batched: [Option<u32>; 16] = [None; 16];
        if mask != 0
            && let Ok(values) = self.interface.read_core_registers(mask)
        {
            for (register, value) in values {
                batched[register as usize] = Some(value);
            }
        }

        addresses
            .iter()
            .map(|&address| {
                if address.0 <= 15
                    && let Some(value) = batched[address.0 as usize]
                {
                    return Ok(RegisterValue::U32(value));
                }
                self.read_core_reg(address)
            })
            .collect()
    }

    fn write_core_reg(&mut self, address: RegisterId, value: RegisterValue) -> Result<(), Error> {
        let value: u32 = value.try_into()?;
        let redirected = address == PC.id && self.interface.cached_halt_pc() != Some(value);
        self.interface.write_core_register(address.0 as u8, value)?;
        if redirected {
            // Redirecting the core at a semihosting halt abandons the call: `run()` must resume
            // at the new PC instead of returning from the SVC. (Servicing a call only writes
            // R0; writing back the unchanged PC, e.g. a debugger writing all registers, keeps
            // the call.) A PC set to the SVC vector again is checked afresh.
            self.state.semihosting_command = None;
            self.state.svc_halt_checked = false;
        }
        Ok(())
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        Ok(Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT as u32)
    }

    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        // A unit holding a data watchpoint is reported as occupied (by the watched address), so
        // `Core::set_hw_breakpoint`'s free-unit search doesn't overwrite it.
        Ok(self
            .state
            .hw_breakpoints
            .iter()
            .zip(&self.state.data_watchpoints)
            .map(|(breakpoint, watchpoint)| breakpoint.or(*watchpoint))
            .collect())
    }

    fn reserved_breakpoint_units(&mut self) -> Result<Vec<Option<u64>>, Error> {
        // Data watchpoints, and the unit holding the SVC vector catch: a user breakpoint at the
        // SVC vector then gets a unit of its own, and clearing it leaves the catch armed.
        let mut reserved = self.state.data_watchpoints.to_vec();
        if self.state.svc_vector_catch_enabled {
            reserved[SVC_VECTOR_UNIT] = Some(SVC_VECTOR_ADDRESS as u64);
        }
        Ok(reserved)
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error> {
        // EmbeddedICE units are enabled individually; there is no global enable to set.
        self.state.breakpoints_enabled = state;
        Ok(())
    }

    fn set_hw_breakpoint(&mut self, unit_index: usize, addr: u64) -> Result<(), Error> {
        // `hw_breakpoints()` reports a data watchpoint as its watched address, so
        // `Core::set_hw_breakpoint` can pick this unit for a breakpoint at that same address,
        // believing it is already set. Refuse instead of silently dropping the watchpoint.
        if let Some(watched) = self
            .state
            .data_watchpoints
            .get(unit_index)
            .copied()
            .flatten()
        {
            return Err(Error::Other(format!(
                "hardware unit {unit_index} holds a data watchpoint at {watched:#010x}; clear \
                 it first"
            )));
        }
        self.interface.set_hw_breakpoint(unit_index, addr as u32)?;
        self.state.hw_breakpoints[unit_index] = Some(addr);
        self.state.data_watchpoints[unit_index] = None;
        if unit_index == SVC_VECTOR_UNIT && addr != SVC_VECTOR_ADDRESS as u64 {
            self.state.svc_vector_catch_enabled = false;
        }
        Ok(())
    }

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), Error> {
        self.interface.clear_hw_breakpoint(unit_index)?;
        self.state.hw_breakpoints[unit_index] = None;
        self.state.data_watchpoints[unit_index] = None;
        if unit_index == SVC_VECTOR_UNIT {
            self.state.svc_vector_catch_enabled = false;
        }
        Ok(())
    }

    fn set_hw_data_watchpoint(&mut self, unit_index: usize, addr: u64) -> Result<(), Error> {
        // Kept apart from `hw_breakpoints`, which holds fetch breakpoints only (used by `run()`).
        self.interface
            .set_hw_data_watchpoint(unit_index, addr as u32)?;
        self.state.hw_breakpoints[unit_index] = None;
        self.state.data_watchpoints[unit_index] = Some(addr);
        if unit_index == SVC_VECTOR_UNIT {
            self.state.svc_vector_catch_enabled = false;
        }
        Ok(())
    }

    fn latch_watchpoint_halt(&mut self) -> Result<(), Error> {
        self.interface.latch_watchpoint_halt()?;
        Ok(())
    }

    fn registers(&self) -> &'static CoreRegisters {
        &ARM7TDMI_CORE_REGISTERS
    }

    fn program_counter(&self) -> &'static CoreRegister {
        &PC
    }

    fn frame_pointer(&self) -> &'static CoreRegister {
        &FP
    }

    fn stack_pointer(&self) -> &'static CoreRegister {
        &SP
    }

    fn return_address(&self) -> &'static CoreRegister {
        &LR
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        self.state.breakpoints_enabled
    }

    fn architecture(&self) -> Architecture {
        Architecture::Arm
    }

    fn core_type(&self) -> CoreType {
        CoreType::Armv4t
    }

    fn instruction_set(&mut self) -> Result<InstructionSet, Error> {
        let cpsr: u32 = self.read_core_reg(CPSR.id)?.try_into()?;
        // CPSR.T. `Thumb2` is the closest tag for Thumb-1, as for Armv6-M.
        match (cpsr >> 5) & 1 {
            1 => Ok(InstructionSet::Thumb2),
            _ => Ok(InstructionSet::A32),
        }
    }

    fn fpu_support(&mut self) -> Result<bool, Error> {
        // ARM7TDMI does not have FPU
        Ok(false)
    }

    fn floating_point_register_count(&mut self) -> Result<usize, Error> {
        Ok(0)
    }

    #[doc(hidden)]
    fn reset_catch_set(&mut self) -> Result<(), Error> {
        self.sequence.reset_catch_set(&mut self.interface)
    }

    #[doc(hidden)]
    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        self.sequence.reset_catch_clear(&mut self.interface)
    }

    #[doc(hidden)]
    fn debug_core_stop(&mut self) -> Result<(), Error> {
        self.sequence.debug_core_stop(&mut self.interface)
    }

    /// Only [`VectorCatchCondition::Svc`] is supported, via `SVC_VECTOR_UNIT`. The other
    /// conditions need a vector-catch register EmbeddedICE doesn't have (and ARM7TDMI has no
    /// `HLT`), so they return `Error::NotImplemented`.
    fn enable_vector_catch(&mut self, condition: VectorCatchCondition) -> Result<(), Error> {
        if condition != VectorCatchCondition::Svc {
            return Err(Error::NotImplemented("vector catch"));
        }
        if self.state.svc_vector_catch_enabled {
            return Ok(());
        }
        if let Some(existing) = self.state.hw_breakpoints[SVC_VECTOR_UNIT] {
            if existing != SVC_VECTOR_ADDRESS as u64 {
                return Err(Error::Other(format!(
                    "hardware breakpoint unit {SVC_VECTOR_UNIT} is already in use (address \
                     {existing:#x}); ARM7TDMI's SVC vector catch needs it and there is no way \
                     to relocate it to the other unit"
                )));
            }
            // A user breakpoint at the SVC vector itself: move it to the other unit if that is
            // free, so it stays separately clearable once the catch reserves this one.
            let other = 1 - SVC_VECTOR_UNIT;
            if self.state.hw_breakpoints[other].is_none()
                && self.state.data_watchpoints[other].is_none()
            {
                self.set_hw_breakpoint(other, SVC_VECTOR_ADDRESS as u64)?;
            }
        }
        self.set_hw_breakpoint(SVC_VECTOR_UNIT, SVC_VECTOR_ADDRESS as u64)?;
        self.state.svc_vector_catch_enabled = true;
        Ok(())
    }

    fn disable_vector_catch(&mut self, condition: VectorCatchCondition) -> Result<(), Error> {
        if condition != VectorCatchCondition::Svc {
            return Err(Error::NotImplemented("vector catch"));
        }
        if !self.state.svc_vector_catch_enabled {
            return Ok(());
        }
        self.clear_hw_breakpoint(SVC_VECTOR_UNIT)?;
        self.state.svc_vector_catch_enabled = false;
        self.state.semihosting_command = None;
        self.state.svc_halt_checked = false;
        Ok(())
    }
}

/// Reject a memory access that isn't naturally aligned: the core's LDM/STM-based accesses would
/// silently ignore the low address bits.
fn check_alignment(address: u64, alignment: usize) -> Result<(), Error> {
    if !address.is_multiple_of(alignment as u64) {
        return Err(MemoryNotAlignedError { address, alignment }.into());
    }
    Ok(())
}

impl<'probe> MemoryInterface for Arm7tdmi<'probe> {
    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_word_64(&mut self, _address: u64) -> Result<u64, Error> {
        Err(Error::Other(
            "ARM7TDMI does not support 64-bit access".to_string(),
        ))
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, Error> {
        check_alignment(address, 4)?;
        let address = valid_32bit_address(address)?;
        Ok(self.interface.read_memory_32(address)?)
    }

    fn read_word_16(&mut self, address: u64) -> Result<u16, Error> {
        check_alignment(address, 2)?;
        let address = valid_32bit_address(address)?;
        let word = self.interface.read_memory_32(address & !0x3)?;
        let offset = (address & 0x3) * 8;
        Ok(((word >> offset) & 0xFFFF) as u16)
    }

    fn read_word_8(&mut self, address: u64) -> Result<u8, Error> {
        let address = valid_32bit_address(address)?;
        let word = self.interface.read_memory_32(address & !0x3)?;
        let offset = (address & 0x3) * 8;
        Ok(((word >> offset) & 0xFF) as u8)
    }

    fn read_64(&mut self, _address: u64, _data: &mut [u64]) -> Result<(), Error> {
        Err(Error::Other(
            "ARM7TDMI does not support 64-bit access".to_string(),
        ))
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        // The burst form avoids per-word `STICKY_HALT` toggles (see `write_memory_32_bulk`).
        check_alignment(address, 4)?;
        let address = valid_32bit_address(address)?;
        let values = self.interface.read_memory_32_bulk(address, data.len())?;
        data.copy_from_slice(&values);
        Ok(())
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), Error> {
        // Aligned pairs go through `read_memory_32_bulk`; only an unaligned leading/trailing
        // halfword uses `read_word_16`.
        if data.is_empty() {
            return Ok(());
        }

        check_alignment(address, 2)?;
        let mut address = valid_32bit_address(address)?;
        let mut data = data;

        if address & 0x3 != 0 {
            data[0] = self.read_word_16(address as u64)?;
            address = address.wrapping_add(2);
            data = &mut data[1..];
        }

        let paired = data.len() / 2;
        if paired > 0 {
            let words = self.interface.read_memory_32_bulk(address, paired)?;
            for (i, word) in words.into_iter().enumerate() {
                data[i * 2] = (word & 0xFFFF) as u16;
                data[i * 2 + 1] = (word >> 16) as u16;
            }
            address = address.wrapping_add((paired as u32) * 4);
            data = &mut data[paired * 2..];
        }

        if let Some(trailing) = data.first_mut() {
            *trailing = self.read_word_16(address as u64)?;
        }

        Ok(())
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        // Aligned words go through `read_memory_32_bulk`; only up to 3 bytes at each end use
        // `read_word_8`.
        if data.is_empty() {
            return Ok(());
        }

        let mut address = valid_32bit_address(address)?;
        let mut data = data;

        while address & 0x3 != 0 && !data.is_empty() {
            data[0] = self.read_word_8(address as u64)?;
            address = address.wrapping_add(1);
            data = &mut data[1..];
        }

        let full_words = data.len() / 4;
        if full_words > 0 {
            let words = self.interface.read_memory_32_bulk(address, full_words)?;
            for (i, word) in words.into_iter().enumerate() {
                data[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
            }
            address = address.wrapping_add((full_words as u32) * 4);
            data = &mut data[full_words * 4..];
        }

        for byte in data.iter_mut() {
            *byte = self.read_word_8(address as u64)?;
            address = address.wrapping_add(1);
        }

        Ok(())
    }

    fn write_word_64(&mut self, _address: u64, _data: u64) -> Result<(), Error> {
        Err(Error::Other(
            "ARM7TDMI does not support 64-bit access".to_string(),
        ))
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        check_alignment(address, 4)?;
        let address = valid_32bit_address(address)?;
        self.interface.write_memory_32(address, data)?;
        Ok(())
    }

    fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), Error> {
        check_alignment(address, 2)?;
        let address = valid_32bit_address(address)?;
        let aligned_addr = address & !0x3;
        let word = self.interface.read_memory_32(aligned_addr)?;
        let offset = (address & 0x3) * 8;
        let mask = !(0xFFFF << offset);
        let new_word = (word & mask) | ((data as u32) << offset);
        self.interface.write_memory_32(aligned_addr, new_word)?;
        Ok(())
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), Error> {
        let address = valid_32bit_address(address)?;
        let aligned_addr = address & !0x3;
        let word = self.interface.read_memory_32(aligned_addr)?;
        let offset = (address & 0x3) * 8;
        let mask = !(0xFF << offset);
        let new_word = (word & mask) | ((data as u32) << offset);
        self.interface.write_memory_32(aligned_addr, new_word)?;
        Ok(())
    }

    fn write_64(&mut self, _address: u64, _data: &[u64]) -> Result<(), Error> {
        Err(Error::Other(
            "ARM7TDMI does not support 64-bit access".to_string(),
        ))
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error> {
        check_alignment(address, 4)?;
        let address = valid_32bit_address(address)?;
        // The burst form, see `write_memory_32_bulk` (per-word `STICKY_HALT` toggles drift PC).
        self.interface.write_memory_32_bulk(address, data)?;
        Ok(())
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), Error> {
        // `write_word_16` is a read-modify-write of two system-speed accesses, so aligned pairs
        // go through `write_memory_32_bulk` (see there); only an unaligned leading/trailing
        // halfword uses the read-modify-write path.
        if data.is_empty() {
            return Ok(());
        }

        check_alignment(address, 2)?;
        let mut address = valid_32bit_address(address)?;
        let mut data = data;

        if address & 0x3 != 0 {
            self.write_word_16(address as u64, data[0])?;
            address = address.wrapping_add(2);
            data = &data[1..];
        }

        let paired = data.len() / 2;
        if paired > 0 {
            let words: Vec<u32> = data[..paired * 2]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| (pair[0] as u32) | ((pair[1] as u32) << 16))
                .collect();
            self.interface.write_memory_32_bulk(address, &words)?;
            address = address.wrapping_add((paired as u32) * 4);
            data = &data[paired * 2..];
        }

        if let Some(&trailing) = data.first() {
            self.write_word_16(address as u64, trailing)?;
        }

        Ok(())
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        // As in `write_16`: aligned words go through `write_memory_32_bulk`, only up to 3 bytes
        // at each end use the `write_word_8` read-modify-write.
        if data.is_empty() {
            return Ok(());
        }

        let mut address = valid_32bit_address(address)?;
        let mut data = data;

        while address & 0x3 != 0 && !data.is_empty() {
            self.write_word_8(address as u64, data[0])?;
            address = address.wrapping_add(1);
            data = &data[1..];
        }

        let full_words = data.len() / 4;
        if full_words > 0 {
            let words: Vec<u32> = data[..full_words * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            self.interface.write_memory_32_bulk(address, &words)?;
            address = address.wrapping_add((full_words as u32) * 4);
            data = &data[full_words * 4..];
        }

        for &byte in data {
            self.write_word_8(address as u64, byte)?;
            address = address.wrapping_add(1);
        }

        Ok(())
    }

    // The default `write()` already splits into an aligned `write_32` burst plus up to 3 bytes
    // at each end via `write_8`; flash page buffers are always whole words.

    fn supports_8bit_transfers(&self) -> Result<bool, Error> {
        Ok(true)
    }

    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
