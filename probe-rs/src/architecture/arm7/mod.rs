//! ARM7TDMI architecture support

use crate::{
    Architecture, CoreInformation, CoreInterface, CoreRegister, CoreStatus, CoreType, Error,
    HaltReason, InstructionSet, MemoryInterface,
    core::{BreakpointCause, CoreRegisters, RegisterId, RegisterValue, VectorCatchCondition},
    memory::valid_32bit_address,
    semihosting::{SemihostingCommand, check_for_semihosting},
};
use std::sync::Arc;
use std::time::Duration;

pub mod communication_interface;
pub mod registers;
pub mod sequences;
mod step_sim;

pub use communication_interface::{
    Arm7tdmiCommunicationInterface, Arm7tdmiDebugInterfaceState, Arm7tdmiError,
};
use registers::*;
pub use sequences::{Arm7tdmiDebugSequence, DefaultArm7tdmiSequence};

/// The hardware breakpoint unit reserved for `VectorCatchCondition::Svc` (see
/// [`Arm7tdmi::enable_vector_catch`]) while it is enabled, leaving the other unit free for
/// [`Arm7tdmi::step`]'s scratch use and ordinary user breakpoints.
///
/// ARM7TDMI's EmbeddedICE debug module has no vector-catch register (no DBGVCR equivalent, as
/// ARMv7-A/R has); this reserves one of the chip's [`Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT`]
/// (2) hardware breakpoint comparators to catch the SVC vector instead. It is tracked through
/// the same `Arm7tdmiState::hw_breakpoints`/`Core::set_hw_breakpoint` bookkeeping as an ordinary
/// breakpoint, so `Core::set_hw_breakpoint`'s free-slot search (which reads
/// [`CoreInterface::hw_breakpoints`]) naturally avoids colliding with it once reserved.
const SVC_VECTOR_UNIT: usize = 0;

/// Address of the SVC exception vector. ARM7TDMI, unlike ARMv6+, has no vector table
/// relocation (no high/low vector control bit), so this is always where `SVC`/`SWI`
/// instructions vector to.
const SVC_VECTOR_ADDRESS: u32 = 0x08;

/// ARM7TDMI core state
#[derive(Debug)]
pub struct Arm7tdmiState {
    initialized: bool,
    current_state: CoreStatus,
    breakpoints_enabled: bool,
    hw_breakpoints: [Option<u64>; Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT],
    /// Whether `enable_vector_catch(VectorCatchCondition::Svc)` is currently active.
    svc_vector_catch_enabled: bool,
    /// The semihosting command decoded at the current SVC-vector-catch halt, if any. Cached so
    /// repeated `status()` calls (see `poll_once` in the `probe-rs run`/`test` run loop) don't
    /// re-decode, and consumed by `run()` to know a semihosting-call return sequence is needed.
    semihosting_command: Option<SemihostingCommand>,
}

impl Arm7tdmiState {
    /// Create a new ARM7TDMI core state
    pub fn new() -> Self {
        Self {
            initialized: false,
            current_state: CoreStatus::Unknown,
            breakpoints_enabled: false,
            hw_breakpoints: [None; Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT],
            svc_vector_catch_enabled: false,
            semihosting_command: None,
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

impl<'probe> Arm7tdmi<'probe> {
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
            // Real, physical TAP reset/IDCODE-read/watchpoint-clear - see
            // `Arm7tdmiCommunicationInterface::new`'s doc comment for why this must happen here,
            // gated on this session-persisted flag, rather than unconditionally in the
            // interface's own constructor (which runs on every `Session::core` call, not just
            // the first).
            core.interface.init()?;
            core.sequence.debug_core_start(&mut core.interface)?;
            // Initialize debug interface. Caches the real, freshly-halted PC (and, if halted in
            // Thumb state, CPSR) for a later bare `resume()` to reuse - see `halt()`'s own doc
            // comment for why this matters specifically for the plain `read`/`write` command
            // path (attach -> this one-time halt -> the memory op -> an implicit `run()`/resume
            // with no explicit PC write in between).
            core.interface.halt()?;
        }

        Ok(core)
    }
}

impl<'probe> CoreInterface for Arm7tdmi<'probe> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            // Must go through `status()`, not a bare `self.interface.is_halted()` - only
            // `status()` calls `latch_watchpoint_halt()` (which performs the required
            // Thumb-to-ARM conversion via `enter_debug_state`) on a fresh halt transition. Every
            // generic `Core::wait_for_core_halted` caller (this crate's own CLI/RPC
            // flashing-completion poll loops, or any plain library caller) relies on this to
            // leave the core genuinely ready for a subsequent chain-1 (ARM-encoded) register
            // read - without it, a watchpoint match on Thumb-mode code would still report
            // "halted" while the core is left mid-Thumb-decode, so every register read would
            // misdecode as garbage.
            if matches!(self.status()?, CoreStatus::Halted(_)) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        Err(Error::Timeout)
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        // See `wait_for_core_halted`'s doc comment just above - same reasoning applies here:
        // this must observe a fresh halt through `status()` so a watchpoint-triggered halt gets
        // durably latched and Thumb-converted before any caller trusts this `true` result and
        // proceeds straight to a chain-1 register/memory access.
        Ok(matches!(self.status()?, CoreStatus::Halted(_)))
    }

    fn status(&mut self) -> Result<CoreStatus, Error> {
        let is_halted = self.interface.is_halted()?;

        if !is_halted {
            self.state.current_state = CoreStatus::Running;
            return Ok(self.state.current_state);
        }

        // A fresh transition into the halted state must be durably latched before any further
        // chain-1 (debug-speed) access is attempted - see `latch_watchpoint_halt`'s own doc
        // comment: a watchpoint-triggered halt reporting DBGACK here does not, on its own,
        // durably stop the core on this silicon, and a caller that proceeds straight to chain-1
        // reads/writes (this function's own SVC-vector-catch check just below, and - critically -
        // any *external* caller that only ever sees this generic `status()` and then reads
        // registers, e.g. over RPC, with no knowledge of `latch_watchpoint_halt` at all) can end
        // up reading/writing a pipeline the core silently isn't actually listening to. Gated on
        // "wasn't already known-halted" so a `Core::halt()`-initiated halt (already durably
        // latched by its own sequence) doesn't pay this twice; calling it again in that case
        // would be harmless (the same idempotent DBGRQ-then-STICKY_HALT sequence `halt()` itself
        // uses) but is unnecessary JTAG traffic.
        let fresh_halt = !matches!(self.state.current_state, CoreStatus::Halted(_));
        if fresh_halt {
            self.interface.latch_watchpoint_halt()?;
        }

        // Only reclassify `reason` on a fresh transition into halted - EmbeddedICE genuinely
        // gives no reason info for one, unlike ARMv7-A/R's DBGDSCR, so `Unknown` is the correct
        // starting point there. On a repeated poll of an already-halted core, keep whatever
        // reason was already known (e.g. the explicit `HaltReason::Request`/`HaltReason::Step`
        // `halt()`/`step()` set) instead of resetting it - a background status-watcher (like the
        // DAP server's own poll loop) calls this repeatedly while halted and must keep seeing a
        // consistent reason. The SVC-vector-catch check below can still refine/override it either
        // way - that check's own semihosting-command caching is deliberately designed to run
        // again on repeated polls of the same halt, see `Arm7tdmiState::semihosting_command`'s
        // own doc.
        let mut reason = if fresh_halt {
            HaltReason::Unknown
        } else if let CoreStatus::Halted(existing) = self.state.current_state {
            existing
        } else {
            HaltReason::Unknown
        };

        // EmbeddedICE's Debug Status Register (unlike ARMv7-A/R's DBGDSCR) doesn't report *why*
        // the core halted, only that it did - so the SVC-vector-catch breakpoint (see
        // `enable_vector_catch`) can only be recognised by checking, after any halt, whether
        // we're both expecting it and sitting exactly at the SVC vector.
        if self.state.svc_vector_catch_enabled {
            // Use the PC `latch_watchpoint_halt` (just above, via `enter_debug_state`) already
            // captured while latching this halt, rather than issuing a second, independent
            // chain-1 read here - see `cached_halt_pc`'s doc comment for why a second read never
            // saw the true halt-time value at all (ROOT CAUSE, 2026-09-08, of this check
            // essentially never matching for a watchpoint hit deep in ROM).
            let pc: u32 = self
                .interface
                .cached_halt_pc()
                .ok_or_else(|| Error::Other("no cached halt PC available".to_string()))?;
            tracing::debug!(
                "SVC vector-catch halt check: pc={pc:#010x} (expected {SVC_VECTOR_ADDRESS:#010x})"
            );
            if pc == SVC_VECTOR_ADDRESS {
                let lr: u32 = self.read_core_reg(LR.id)?.try_into()?;
                self.state.semihosting_command =
                    check_for_semihosting(self.state.semihosting_command.take(), self, pc, lr)?;
                if let Some(command) = self.state.semihosting_command {
                    reason = HaltReason::Breakpoint(BreakpointCause::Semihosting(command));
                }
            }
        }

        self.state.current_state = CoreStatus::Halted(reason);
        Ok(self.state.current_state)
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.interface.halt()?;
        self.wait_for_core_halted(timeout)?;

        // `wait_for_core_halted` goes through `status()`, which has no way to distinguish *why*
        // the core is halted - EmbeddedICE's Debug Status Register, unlike ARMv7-A/R's DBGDSCR,
        // never reports a halt reason, only that a halt happened. This trait method, though, is
        // unambiguous about *why* the core is halted: it's always this crate/a debugger
        // explicitly requesting a halt (never a watchpoint/breakpoint match, which goes through
        // `status()`'s own polling instead) - matching every real call site (`Session::halted_access`,
        // flash-prep, `benchmark`, a DAP/REPL `break`/pause command, ...). Set it explicitly, the
        // same way `step()` already does for its own, differently-caused halt.
        self.state.current_state = CoreStatus::Halted(HaltReason::Request);

        let pc: u32 = self.read_core_reg(PC.id)?.try_into()?;
        // Avoid a later, redundant chain-1 PC re-read in `resume()` - see `cache_resume_pc`.
        self.interface.cache_resume_pc(pc);

        Ok(CoreInformation { pc: pc as u64 })
    }

    fn run(&mut self) -> Result<(), Error> {
        if self.state.semihosting_command.take().is_some() {
            // We're halted right at the SVC vector (Supervisor mode), having intercepted a
            // semihosting call. Return to the interrupted code exactly as the `SVC`
            // instruction itself would have, had the debugger not caught it first.
            self.interface.return_from_exception()?;
        }

        self.interface.resume()?;
        self.state.current_state = CoreStatus::Running;
        Ok(())
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.sequence.reset_system(&mut self.interface)?;
        // A stale cached command would make the next `run()` try to `return_from_exception`
        // out of an exception mode the reset already took us out of.
        self.state.semihosting_command = None;
        // A real hardware reset (a physical reset-line toggle, not just a PC redirect) clears
        // EmbeddedICE's watchpoint comparator registers on the actual silicon along with
        // everything else - so any breakpoint/vector-catch programmed before this reset is now
        // gone on the real hardware, even though this cached bookkeeping doesn't know that yet.
        // Left stale (as this used to be), `enable_vector_catch`'s own "already enabled, nothing
        // to do" fast path (and any user `set_hw_breakpoint` caller relying on `hw_breakpoints`
        // to know what's actually armed) would wrongly believe the breakpoint is still active
        // and never reprogram it - letting e.g. a semihosting SVC call silently fall through to
        // the target's own raw SWI vector instead of being caught, with no further debugger
        // involvement at all until some later, unrelated halt.
        self.state.hw_breakpoints =
            [None; Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT];
        self.state.svc_vector_catch_enabled = false;
        self.state.current_state = CoreStatus::Running;
        Ok(())
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.state.semihosting_command = None;
        // See `reset`'s doc comment: a real reset clears EmbeddedICE's breakpoint comparators on
        // the actual hardware, so this cached bookkeeping must not keep claiming they're armed.
        self.state.hw_breakpoints =
            [None; Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT];
        self.state.svc_vector_catch_enabled = false;
        self.sequence.reset_catch_set(&mut self.interface)?;
        self.sequence.reset_system(&mut self.interface)?;
        self.sequence.reset_catch_clear(&mut self.interface)?;

        self.interface.halt()?;
        self.wait_for_core_halted(timeout)?;

        let pc: u32 = self.read_core_reg(PC.id)?.try_into()?;
        // Avoid a later, redundant chain-1 PC re-read in `resume()` - see `cache_resume_pc`.
        self.interface.cache_resume_pc(pc);

        Ok(CoreInformation { pc: pc as u64 })
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        // Calculate the address the next instruction will genuinely execute from, by
        // decoding/simulating the current instruction (handling conditional execution and any
        // control-flow redirection: branches, BX, and any instruction that writes PC directly)
        // - the same technique OpenOCD's own ARM7/ARM9 `step` uses (`arm7_9_step`/
        // `arm_simulate_step`). See `step_sim`'s module doc for why a naive
        // "breakpoint at PC + instruction size" approach would get this wrong for any taken
        // branch.
        //
        // Both hardware watchpoint units are temporarily repurposed as a "range-chained" pair
        // (see `Arm7tdmiCommunicationInterface::configure_step_watchpoints`) rather than a lone
        // wildcard or exact-match breakpoint on one unit: a target only one instruction ahead
        // of where `resume()` redirects execution does not reliably fire a lone watchpoint on
        // real ARM7TDMI silicon (observed directly: PC advancing by a large, fixed, multi-
        // instruction amount per `step()` call instead of stopping after exactly one). Any
        // breakpoints the user had set on either unit are restored afterwards.
        let (current_pc, next_pc) = step_sim::calculate_next_pc(&mut self.interface)?;

        let previous = self.state.hw_breakpoints;
        self.interface
            .configure_step_watchpoints(current_pc, next_pc)?;

        self.interface.resume()?;
        // Uses the DBGACK+SYSCOMP dual check (matching `system_speed_access`/OpenOCD's
        // `arm7_9_execute_sys_speed`, which `arm7_9_step` uses) instead of the generic
        // DBGACK-only `wait_for_core_halted` - DBGACK alone can assert before the core has
        // genuinely run to the intended watchpoint match on this silicon.
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(1);
        let step_result = loop {
            match self.interface.is_halted_with_syscomp() {
                Ok(true) => break Ok(()),
                Ok(false) => {
                    if start.elapsed() >= timeout {
                        break Err(Error::Timeout);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => break Err(e.into()),
            }
        };

        for (unit, addr) in previous.into_iter().enumerate() {
            match addr {
                Some(addr) => self.interface.set_hw_breakpoint(unit, addr as u32)?,
                None => self.interface.clear_hw_breakpoint(unit)?,
            }
        }
        step_result?;

        self.state.current_state = CoreStatus::Halted(HaltReason::Step);

        // Deliberately *not* re-reading PC here to report the result: reading any register
        // via ARM7TDMI's debug-speed chain-1 capture has a real, physical side effect on this
        // silicon - it genuinely advances the core's pipeline by a further ~3 instructions
        // (not just a misleading reported value; a *second* read afterward keeps confirming a
        // further-advanced position each time), a phenomenon this whole investigation
        // documented independently more than once (see `probe_rs_arm7_chain1_bug` memory).
        // Ground-truth verified on real hardware: `next_pc` (computed by `calculate_next_pc`
        // before `resume()`, and the exact address armed via `configure_step_watchpoints`) is
        // consistently exactly 12 bytes (one debug-speed read's worth of nudge) less than
        // whatever a subsequent PC read reports - i.e. the watchpoint mechanism genuinely,
        // correctly lands the core at `next_pc`; a verification read afterward would only add
        // the same artifact on top, and would itself further displace the core from where the
        // caller expects it to be for any subsequent step. Returning `next_pc` directly is both
        // more accurate and avoids compounding this side effect across repeated steps.
        Ok(CoreInformation { pc: next_pc as u64 })
    }

    fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, Error> {
        let value = self.interface.read_core_register(address.0 as u8)?;
        Ok(RegisterValue::U32(value))
    }

    fn read_core_regs_batch(
        &mut self,
        addresses: &[RegisterId],
    ) -> Vec<Result<RegisterValue, Error>> {
        // Fold the plain R0-R15 register ids into one mask and read them all via a single
        // `STMIA` (see `Arm7tdmiCommunicationInterface::read_core_registers`'s doc for why this
        // matters on this architecture). CPSR (16) and anything else outside 0-15 falls back to
        // `read_core_reg` below, same as if this override didn't exist at all.
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
        self.interface.write_core_register(address.0 as u8, value)?;
        Ok(())
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        Ok(Arm7tdmiCommunicationInterface::HW_BREAKPOINT_UNIT_COUNT as u32)
    }

    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        Ok(self.state.hw_breakpoints.to_vec())
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error> {
        // Each EmbeddedICE watchpoint unit is enabled/disabled individually via its own
        // control register (see `set_hw_breakpoint`/`clear_hw_breakpoint`); there is no
        // separate global enable. This just tracks the state `Core::set_hw_breakpoint`
        // expects to be able to query.
        self.state.breakpoints_enabled = state;
        Ok(())
    }

    fn set_hw_breakpoint(&mut self, unit_index: usize, addr: u64) -> Result<(), Error> {
        self.interface.set_hw_breakpoint(unit_index, addr as u32)?;
        self.state.hw_breakpoints[unit_index] = Some(addr);
        Ok(())
    }

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), Error> {
        self.interface.clear_hw_breakpoint(unit_index)?;
        self.state.hw_breakpoints[unit_index] = None;
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
        // CPSR bit 5 - T - Thumb mode. There's no dedicated "Thumb-1" variant in
        // `InstructionSet`; `Thumb2` is used elsewhere in this codebase (e.g. Armv6-M, which is
        // also Thumb-1-only) as the closest available tag for "the core is in Thumb state".
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

    /// Only [`VectorCatchCondition::Svc`] is supported, via `SVC_VECTOR_UNIT` (see its docs).
    ///
    /// `HardFault`/`SecureFault` are Cortex-M-only concepts, and `CoreReset`/`Hlt` would need
    /// their own vector-catch register - EmbeddedICE has none (no DEMCR/DBGVCR equivalent), and
    /// ARM7TDMI has no `HLT` instruction at all. Those conditions keep the `CoreInterface`
    /// default of `Err(Error::NotImplemented("vector catch"))`, which is the correct, honest
    /// behaviour for them - unlike silently returning `Ok(())` while doing nothing.
    fn enable_vector_catch(&mut self, condition: VectorCatchCondition) -> Result<(), Error> {
        if condition != VectorCatchCondition::Svc {
            return Err(Error::NotImplemented("vector catch"));
        }
        if self.state.svc_vector_catch_enabled {
            return Ok(());
        }
        if let Some(existing) = self.state.hw_breakpoints[SVC_VECTOR_UNIT]
            && existing != SVC_VECTOR_ADDRESS as u64
        {
            return Err(Error::Other(format!(
                "hardware breakpoint unit {SVC_VECTOR_UNIT} is already in use (address \
                 {existing:#x}); ARM7TDMI's SVC vector catch needs it and there is no way to \
                 relocate it to the other unit"
            )));
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
        Ok(())
    }
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
        let address = valid_32bit_address(address)?;
        Ok(self.interface.read_memory_32(address)?)
    }

    fn read_word_16(&mut self, address: u64) -> Result<u16, Error> {
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
        // BUG FOUND (2026-09-10): this used to loop calling `read_memory_32` once per word - the
        // same per-word `STICKY_HALT`-toggle drift class already found and fixed for `write_32`
        // (see `write_memory_32_bulk`'s doc comment), just never applied to reads at all. Fixed
        // via `read_memory_32_bulk` - see its own doc comment.
        let address = valid_32bit_address(address)?;
        let values = self.interface.read_memory_32_bulk(address, data.len())?;
        data.copy_from_slice(&values);
        Ok(())
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), Error> {
        // Bulk-pack aligned pairs through `read_memory_32_bulk` (one `STICKY_HALT` toggle per up
        // to 4 words, not one per halfword) - mirrors `write_16`'s own fix (same file). Only a
        // leading/trailing halfword that isn't 4-byte-aligned still uses the individual
        // `read_word_16` path, at most twice total.
        if data.is_empty() {
            return Ok(());
        }

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
        // Bulk-pack aligned 4-byte groups through `read_memory_32_bulk` - mirrors `write_8`'s
        // own fix (same file). Only leading/trailing bytes short of a 4-byte boundary still use
        // the individual `read_word_8` path, at most 3 on each end.
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
        let address = valid_32bit_address(address)?;
        self.interface.write_memory_32(address, data)?;
        Ok(())
    }

    fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), Error> {
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
        let address = valid_32bit_address(address)?;
        // Use the burst form, not a per-word loop calling `write_memory_32` - see
        // `write_memory_32_bulk`'s doc comment for why: a per-word loop toggles the debug
        // control register's `STICKY_HALT` bit off and back on for every single word, which
        // reliably drifts the core's real PC over many words even though each individual write
        // reports success.
        self.interface.write_memory_32_bulk(address, data)?;
        Ok(())
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), Error> {
        // BUG FOUND (2026-09-10): the previous form of this function looped calling
        // `write_word_16` once per halfword - and `write_word_16` is itself a read-modify-write
        // (one `read_memory_32` plus one `write_memory_32`, each its own `system_speed_access`
        // call, i.e. its own independent `STICKY_HALT` clear/restore cycle). A loop over N
        // halfwords therefore did 2N separate STICKY_HALT toggle cycles - exactly the already-
        // documented, already-fixed-for-32-bit drift bug (see `write_32`'s own comment and
        // `write_memory_32_bulk`'s doc comment: "a per-word loop toggles STICKY_HALT off then
        // back on for every single word... reliably drifts the core's real PC... even though
        // each individual write reports success"), just twice as exposed per unit of data
        // written, since a 16-bit write needs both a read and a write where a 32-bit write only
        // ever needed the write. Confirmed as a real, live instance of this drift class: writing
        // 4 individual test halfwords (8 total STICKY_HALT toggles) ahead of a `step()` call left
        // the core's real PC ~400 bytes past where it was halted, by the time `step()` ran.
        //
        // Fixed the same way `write_32` already was: route aligned pairs of halfwords through
        // the already-fixed, single-toggle `write_memory_32_bulk` burst primitive instead of a
        // per-halfword loop. Only a leading/trailing halfword that isn't 4-byte-aligned still
        // needs the individual read-modify-write path - at most twice total, not once per
        // halfword.
        if data.is_empty() {
            return Ok(());
        }

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
        // BUG FOUND (2026-09-10): the same per-word `STICKY_HALT`-toggle drift class already
        // fixed for `write_16` (same file) - a per-byte loop calling `write_word_8`, itself a
        // read-modify-write (2 system-speed accesses per byte). Not currently hit by this
        // project's own real usage (the only large caller, `Flasher::load`'s stack-overflow-check
        // fill, only reaches this path when the flash algorithm's `stack_size` isn't a multiple
        // of 4 - this project's default 512-byte stack takes the already-safe `write_32` branch
        // instead), but a real, exposed bug for any other caller with a large buffer. Fixed the
        // same way: bulk-pack aligned 4-byte groups through `write_memory_32_bulk`; only
        // leading/trailing bytes short of a 4-byte boundary still use the individual
        // `write_word_8` read-modify-write path, at most 3 on each end.
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

    // Deliberately not overriding `write()`: the default `MemoryInterface::write()`
    // implementation splits a byte slice into an aligned `write_32` bulk (the fast, single-DBGACK-
    // toggle path - see `write_memory_32_bulk`) plus up to 3 leftover bytes on each end via
    // `write_8`, rather than looping `write_8` (and its own per-byte `write_word_8` read-modify-
    // write) over the *entire* buffer the way this used to. `load_page_buffer` (used to stage
    // each page of flashing data into RAM) writes buffers that are always a whole multiple of 4
    // bytes, so this override being removed means it now goes through the aligned bulk path
    // entirely instead of hitting the real PC-drift bug documented on `write_memory_32_bulk`.

    fn supports_8bit_transfers(&self) -> Result<bool, Error> {
        Ok(true)
    }

    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
