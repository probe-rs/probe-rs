//! ARM7TDMI JTAG communication interface via EmbeddedICE
//!
//! This module implements the JTAG-based debugging protocol for ARM7TDMI cores
//! using the EmbeddedICE debug module, via scan chain 1 (data/instruction bus,
//! used to single-step the core's 3-stage pipeline and observe/inject values on
//! its data bus) and scan chain 2 (the EmbeddedICE register file itself, which
//! holds the debug control/status registers and the watchpoint/breakpoint units).
//!
//! Protocol details are taken from ARM DDI 0029G ("ARM7TDMI (Rev 3) Technical
//! Reference Manual"), Appendix B "Debug in Depth", and cross-checked against
//! OpenOCD's `src/target/arm7tdmi.c`, `arm7_9_common.c` and `embeddedice.c`,
//! which implement the same protocol and are cited by name in the comments
//! below wherever a specific detail comes from them.

use crate::{
    Error,
    architecture::arm::ArmError,
    probe::{BitSequence, DebugProbeError, JtagBatch, JtagChain, TapState},
};

use bitvec::field::BitField;
use std::fmt;
use std::time::{Duration, Instant};

/// The length, in bits, of the ARM7TDMI's own JTAG instruction register - this project's board
/// has exactly one TAP (the ARM7TDMI itself), so [`JtagChain`]'s automatic chain-padding around
/// this is always zero.
const IR_LEN: usize = 4;

/// ARM7TDMI JTAG instruction codes (4-bit IR)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum JtagInstruction {
    /// EXTEST - External boundary scan test
    Extest = 0x0,
    /// SCAN_N - Scan chain select
    ScanN = 0x2,
    /// SAMPLE/PRELOAD - Sample/preload boundary scan
    Sample = 0x3,
    /// RESTART - Restart the processor (run at system speed until it re-enters debug state)
    Restart = 0x4,
    /// CLAMP - Drive outputs from boundary scan register
    Clamp = 0x5,
    /// HIGHZ - Put outputs in high-impedance state
    Highz = 0x7,
    /// CLAMPZ - CLAMP + HIGHZ
    Clampz = 0x9,
    /// INTEST - Internal boundary scan test (used to access scan chain 1/2 register content)
    Intest = 0xC,
    /// IDCODE - Read device ID code
    Idcode = 0xE,
    /// BYPASS - Bypass register (1-bit)
    Bypass = 0xF,
}

/// ARM7TDMI scan chain numbers
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum ScanChain {
    /// Scan chain 0: full boundary scan (not used for debugging)
    Chain0 = 0,
    /// Scan chain 1: data bus, instruction bus and the BREAKPT bit
    Chain1 = 1,
    /// Scan chain 2: EmbeddedICE registers
    Chain2 = 2,
}

/// EmbeddedICE register addresses (accessed via scan chain 2)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
enum EmbeddedIceRegister {
    /// Debug Control Register
    DebugControl = 0,
    /// Debug Status Register
    DebugStatus = 1,
    /// Debug Comms Control Register
    DebugCommsControl = 4,
    /// Debug Comms Data Register
    DebugCommsData = 5,
    /// Watchpoint 0 Address Value
    Watchpoint0AddressValue = 8,
    /// Watchpoint 0 Address Mask
    Watchpoint0AddressMask = 9,
    /// Watchpoint 0 Data Value
    Watchpoint0DataValue = 10,
    /// Watchpoint 0 Data Mask
    Watchpoint0DataMask = 11,
    /// Watchpoint 0 Control Value
    Watchpoint0ControlValue = 12,
    /// Watchpoint 0 Control Mask
    Watchpoint0ControlMask = 13,
    /// Watchpoint 1 Address Value
    Watchpoint1AddressValue = 16,
    /// Watchpoint 1 Address Mask
    Watchpoint1AddressMask = 17,
    /// Watchpoint 1 Data Value
    Watchpoint1DataValue = 18,
    /// Watchpoint 1 Data Mask
    Watchpoint1DataMask = 19,
    /// Watchpoint 1 Control Value
    Watchpoint1ControlValue = 20,
    /// Watchpoint 1 Control Mask
    Watchpoint1ControlMask = 21,
}

impl EmbeddedIceRegister {
    /// The four registers making up hardware watchpoint/breakpoint unit `index` (0 or 1).
    fn watchpoint_unit(index: usize) -> Option<(Self, Self, Self, Self)> {
        match index {
            0 => Some((
                Self::Watchpoint0AddressValue,
                Self::Watchpoint0AddressMask,
                Self::Watchpoint0DataMask,
                Self::Watchpoint0ControlValue,
            )),
            1 => Some((
                Self::Watchpoint1AddressValue,
                Self::Watchpoint1AddressMask,
                Self::Watchpoint1DataMask,
                Self::Watchpoint1ControlValue,
            )),
            _ => None,
        }
    }

    fn control_mask_register(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Watchpoint0ControlMask),
            1 => Some(Self::Watchpoint1ControlMask),
            _ => None,
        }
    }
}

/// Debug Status Register bits.
///
/// Confirmed against OpenOCD's actual `embeddedice.h` (`EICE_DBG_STATUS_*`): `SYSCOMP` and
/// `IFEN` were swapped here (bit 2 is `IFEN`, bit 3 is `SYSCOMP`, not the other way around) -
/// every status value observed all session (e.g. `0x1019`, `0x9`) has bit 3 set, meaning
/// system-speed access was completing successfully the whole time; `system_speed_access` was
/// just polling the wrong bit (its mislabeled `SYSCOMP` at bit 2, which is really `IFEN` and was
/// never set) and spinning for the full timeout regardless.
#[allow(dead_code)]
mod debug_status {
    /// Core is in debug state
    pub const DBGACK: u32 = 1 << 0;
    /// Debug request
    pub const DBGRQ: u32 = 1 << 1;
    /// Instruction fetch enable (OpenOCD's `EICE_DBG_STATUS_IFEN`)
    pub const IFEN: u32 = 1 << 2;
    /// System speed access completed (OpenOCD's `EICE_DBG_STATUS_SYSCOMP`)
    pub const SYSCOMP: u32 = 1 << 3;
    /// Core is in Thumb state (OpenOCD's `EICE_DBG_STATUS_ITBIT`)
    pub const TBIT: u32 = 1 << 4;
}

/// Debug Control Register bits
#[allow(dead_code)]
mod debug_control {
    /// A "sticky halt" latch bit (OpenOCD's `EICE_DBG_CONTROL_DBGACK`, confusingly the same
    /// name as the *Debug Status* register's bit 0, but a different, control-side bit).
    ///
    /// Set right after halting (alongside clearing `DBGRQ`) so the core stays in debug state
    /// without DBGRQ having to remain asserted. It must be cleared before a system-speed
    /// access is attempted (see `system_speed_access`) - while set, it holds the core in
    /// debug state regardless of `RESTART`, so `RESTART` has no effect and `SYSCOMP` never
    /// gets set - and restored afterward.
    pub const STICKY_HALT: u32 = 1 << 0;
    /// Debug request bit
    pub const DBGRQ: u32 = 1 << 1;
    /// Interrupt disable bit
    pub const INTDIS: u32 = 1 << 2;
}

/// Watchpoint control register bits (per ARM DDI 0029G Appendix B / OpenOCD's
/// `embeddedice.h`).
#[allow(dead_code)]
mod watchpoint_control {
    /// Enable this watchpoint unit.
    pub const ENABLE: u32 = 0x100;
    pub const RANGE: u32 = 0x80;
    pub const CHAIN: u32 = 0x40;
    pub const EXTERN: u32 = 0x20;
    pub const N_TRANS: u32 = 0x10;
    /// Active low: 0 = opcode fetch (instruction breakpoint), 1 = data access.
    pub const N_OPC: u32 = 0x8;
    pub const MAS: u32 = 0x6;
    pub const N_RW: u32 = 0x1;
}

/// ARM7TDMI instructions used to single-step the pipeline via scan chain 1.
///
/// The debugger controls the core one clock at a time while it is halted: shifting a value
/// through scan chain 1 and pulsing Update-DR clocks the core once. A single-word data
/// transfer instruction (`LDR`/`STR`) takes 4 such clocks before its result is available:
/// fetch, decode, execute (address calculation), and the data-transfer bus cycle itself,
/// during which the debugger can either observe (capture) or drive (inject) the value on
/// the data bus. See ARM DDI 0029G Appendix B and OpenOCD's `arm7tdmi.c`
/// (`arm7tdmi_clock_out`/`arm7tdmi_clock_data_in`) for the equivalent technique.
mod arm7_instructions {
    /// MOV r0, r0 (NOP)
    pub const NOP: u32 = 0xE1A0_0000;
    /// `LDMIA R0, {<reglist>}` template (increment-after load-multiple, base register R0, no
    /// write-back, no S-bit) - OR in `(1 << register)` to select which register loads. This is
    /// how a single register gets an arbitrary literal injected without a real memory access:
    /// matches OpenOCD's `arm7tdmi_write_core_regs` (used with a single-bit mask for exactly
    /// this, e.g. `arm7_9_write_memory`'s `reg[0] = address; write_core_regs(target, 0x1,
    /// reg)`).
    pub const LOAD_MULTIPLE_R0: u32 = 0xE890_0000;
    /// `STMIA R0, {<reglist>}` template - the capture counterpart of `LOAD_MULTIPLE_R0`, no
    /// write-back. Matches OpenOCD's `arm7tdmi_read_core_regs`, used to read back a register's
    /// value (even a single one - see `read_core_register`).
    pub const STORE_MULTIPLE_R0: u32 = 0xE880_0000;
    /// `LDMIA R0!, {<reglist>}` template - like `LOAD_MULTIPLE_R0` but *with* write-back
    /// (base register R0 auto-increments). Matches OpenOCD's `arm7tdmi_load_word_regs`
    /// (`ARMV4_5_LDMIA(0, mask, 0, 1)`, the last `1` being the write-back flag), used to queue
    /// a *real* system-speed memory read (see `read_memory_32`) - OpenOCD never uses a
    /// single-word `LDR` for this, only this write-back LDM.
    pub const LOAD_MULTIPLE_R0_WRITEBACK: u32 = 0xE8B0_0000;
    /// `STMIA R0!, {<reglist>}` template - the store counterpart of
    /// `LOAD_MULTIPLE_R0_WRITEBACK`, matching OpenOCD's `arm7tdmi_store_word_regs`. Used for a
    /// real system-speed memory write (see `write_memory_32`).
    pub const STORE_MULTIPLE_R0_WRITEBACK: u32 = 0xE8A0_0000;
    /// MRS R0, CPSR
    pub const MRS_R0_CPSR: u32 = 0xE10F_0000;
    /// MRS R0, SPSR - reads the banked Saved Program Status Register of whatever exception
    /// mode the core is currently in (e.g. `SPSR_svc` right after a real SVC exception entry) -
    /// the exact CPSR value the interrupted code had, captured automatically by hardware at
    /// exception entry. Same encoding as `MRS_R0_CPSR` with the `R` bit (22) set to select SPSR
    /// instead of CPSR. Used by
    /// [`super::Arm7tdmiCommunicationInterface::return_from_exception`] to learn the real
    /// resume state (mode/flags/T) without needing to execute a real, hardware exception-return
    /// instruction to find out.
    pub const MRS_R0_SPSR: u32 = 0xE14F_0000;
    /// `STR R0, [R15]` - matches OpenOCD's `arm7tdmi_read_xpsr`'s `ARMV4_5_STR(0, 15)`. Used
    /// only to capture a value already loaded into R0 by a preceding instruction (currently just
    /// `MRS_R0_CPSR`) *instead of* the usual `STORE_MULTIPLE_R0 | 1` (`STMIA R0, {R0}`)
    /// self-referential trick: that trick uses R0's *own* value as the store's memory base
    /// address, which is fine for a normal register read (R0 usually holds a plausible pointer
    /// from real program state) but not for CPSR - CPSR's bit pattern is essentially never
    /// 4-byte aligned, and R15/PC (always word-aligned during ARM execution) is exactly why
    /// OpenOCD deliberately does not reuse its own R0-self-store idiom here.
    pub const STR_R0_R15: u32 = 0xE58F_0000;
    /// MSR CPSR_<field>, #(imm8 ROR rotate_imm*2) template.
    /// OR in `(field_mask << 16) | (rotate_imm << 8) | imm8`.
    pub const MSR_CPSR_IMM: u32 = 0xE320_F000;
    /// `BX R0` - a genuine interworking branch: sets `PC = R0 & !1` and the CPSR T bit from
    /// R0's bit 0, atomically, as its documented architectural side effect. Used by
    /// [`super::Arm7tdmiCommunicationInterface::branch_resume_thumb_aware`] to resume into
    /// Thumb-mode code, matching OpenOCD's `arm7tdmi_branch_resume_thumb` exactly - this is the
    /// *only* mechanism that correctly performs Thumb interworking on ARMv4T; a plain `B`
    /// (`BRANCH_BACK_TO_PC`) cannot.
    pub const BX_R0: u32 = 0xE12F_FF10;
    /// `B PC-16` (branch back by 4 words, i.e. `B` with a 24-bit signed word-offset of -6) -
    /// matches OpenOCD's `arm7tdmi_branch_resume` (`ARMV4_5_B(0xfffffa, 0)` =
    /// `0xea000000 | 0xfffffa` = this value). Injected via debug-speed clocking immediately
    /// after [`super::Arm7tdmiCommunicationInterface::write_pc`], with no other scan-chain-1
    /// operation in between: the offset is calibrated for that *exact* clock sequence (a real
    /// ARM branch instruction's target is PC-of-branch+8+offset*4), so it lands back on
    /// whatever value `write_pc` just injected. Real ARM7TDMI silicon needs this genuine,
    /// pipeline-flushing branch to redirect fetch/decode to the new PC before a `RESTART` can
    /// let the core run for real - a debug-speed `LDMIA`-injected PC value alone leaves stale
    /// prefetched instructions in the pipeline, which is what `RESTART` would otherwise
    /// (mis-)execute for a handful of cycles before the core faults/re-halts. See
    /// `write_pc`/`branch_resume`.
    pub const BRANCH_BACK_TO_PC: u32 = 0xEAFF_FFFA;
}

/// Thumb (16-bit) instruction encodings used only by [`Arm7tdmiCommunicationInterface::change_to_arm`]
/// to convert the core out of Thumb state before any ARM-encoded [`arm7_instructions`] sequence
/// is attempted.
///
/// Scan chain 1's data bus is 32 bits wide regardless of core state; when the CPU is decoding
/// Thumb instructions it fetches two 16-bit halfwords per 32-bit bus cycle, so each encoding
/// here duplicates the same 16-bit opcode into both halves - matching OpenOCD's
/// `ARMV4_5_T_*` macros (`arm_opcodes.h`), e.g. `ARMV4_5_T_NOP` is `0x46c0 | (0x46c0 << 16)`.
mod thumb_instructions {
    /// MOV r8, r8 (NOP), duplicated into both halfwords.
    pub const NOP: u32 = 0x46C0_46C0;
    /// STR r0, [r0], duplicated into both halfwords.
    pub const STR_R0_R0: u32 = 0x6000_6000;
    /// MOV r0, r15 (r0 = pc), duplicated into both halfwords.
    pub const MOV_R0_R15: u32 = 0x4678_4678;
    /// BX pc (branch and exchange to the PC itself) - the ARM7TDMI-S Technical Reference
    /// Manual's own documented Thumb-to-ARM state switch (`DDI 0234B`, "Determining the core
    /// state"): in Thumb state, reading PC as an operand always yields the current
    /// instruction's address + 4, which is architecturally guaranteed halfword-aligned (bit 0
    /// clear) - so `BX PC` unconditionally switches to ARM state with no register load or
    /// memory access needed, unlike OpenOCD's own `arm7tdmi_change_to_arm`, which instead uses
    /// a PC-relative `LDR` to load a literal 0 into r0 and then `BX r0`. Duplicated into both
    /// halfwords.
    pub const BX_PC: u32 = 0x4778_4778;
    /// `LDR r0, [PC, #0]` (PC-relative literal load, offset 0), duplicated into both halfwords -
    /// matches OpenOCD's `ARMV4_5_T_LDR_PCREL(0)`. Used by
    /// [`super::Arm7tdmiCommunicationInterface::branch_resume_thumb_aware`] to restore r0's real
    /// value (clobbered as the `BX` target/scratch register a few clocks earlier) via the same
    /// "inject an arbitrary literal during the load's data-transfer cycle" trick
    /// [`super::Arm7tdmiCommunicationInterface::load_immediate`] uses in ARM state - needed in
    /// Thumb encoding here since the core has already switched to Thumb state by this point.
    pub const LDR_R0_PCREL: u32 = 0x4800_4800;
    /// `B PC-16` (Thumb unconditional branch, 11-bit signed halfword-offset field `0x7f8` = -8
    /// halfwords), duplicated into both halfwords - matches OpenOCD's `arm7tdmi_branch_resume_thumb`'s
    /// trailing `ARMV4_5_T_B(0x7f8)` exactly, clock-for-clock (see
    /// [`super::Arm7tdmiCommunicationInterface::branch_resume_thumb_aware`]'s doc comment - this
    /// must be injected at the *exact* same clock distance from the preceding sequence as
    /// OpenOCD's own, since - like [`super::arm7_instructions::BRANCH_BACK_TO_PC`] - its offset
    /// is calibrated to land back on the `pc` value already loaded into R0 a few clocks earlier,
    /// not computed dynamically.
    pub const BRANCH_BACK_TO_PC: u32 = 0xE7F8_E7F8;
}

/// The state information for the ARM7TDMI debug interface
#[derive(Debug, Default, Clone)]
pub struct Arm7tdmiDebugInterfaceState {
    /// Whether the core is known to be in debug state
    in_debug_state: bool,
    /// The value most recently written to R15 via [`Arm7tdmiCommunicationInterface::write_core_register`],
    /// if any - consumed by `resume()` to redo the PC write immediately before
    /// [`Arm7tdmiCommunicationInterface::branch_resume`], with no other scan-chain-1 operation
    /// in between (see the note on `resume()`). `None` means PC wasn't explicitly rewritten
    /// since the last resume (a plain "continue" without changing PC), in which case `resume()`
    /// falls back to reading the current architectural PC.
    pending_resume_pc: Option<u32>,
    /// Whether [`Arm7tdmiCommunicationInterface::halt`] found the core halted in Thumb state
    /// (`debug_status::TBIT` set) - `false` for an ARM-state halt.
    ///
    /// `halt()`'s Thumb handling converts the core to ARM state via [`Arm7tdmiCommunicationInterface::change_to_arm`]
    /// (a real `BX pc`, needed before any ARM-encoded chain-1 sequence in this file can be used
    /// at all) and restores the original PC/R0 - but never restores the T bit itself, since
    /// nothing in `halt()` needs the core to actually keep running afterward. `resume()` does:
    /// if this is `true`, a plain `write_pc`+`branch_resume` (a bare ARM `B`, which cannot
    /// perform Thumb interworking and would leave the core in ARM state trying to decode real
    /// Thumb-encoded target code as ARM opcodes - an Undefined Instruction almost immediately)
    /// is not enough; see `resume()`/`branch_resume_thumb_aware`, which uses a real `BX`
    /// instead (ported from OpenOCD's `arm7tdmi_branch_resume_thumb`, the actual, hardware-
    /// verified reference mechanism - an earlier attempt at synthesizing the T bit via a
    /// temporary SVC-mode SPSR write + `MOVS PC, LR` was **not** how any known-working
    /// implementation does this, and reliably corrupted execution on real hardware; OpenOCD
    /// itself explicitly never writes T via CPSR/SPSR MSR at all, always masking it out).
    ///
    /// No CPSR/flags/mode/I-F capture is needed here at all, unlike that abandoned approach:
    /// nothing between the original halt and the eventual `BX` in `branch_resume_thumb_aware`
    /// ever touches CPSR (`change_to_arm`'s own `BX pc` only ever changes T, same as any `BX`),
    /// so mode/flags/I/F are naturally still exactly what they were at the real original halt -
    /// only T itself needs restoring, which `BX`'s own architectural effect (T taken from the
    /// branch target register's bit 0) does atomically as part of redirecting execution.
    pending_resume_thumb: bool,
}

impl Arm7tdmiDebugInterfaceState {
    /// Create a new ARM7TDMI debug interface state
    pub fn new() -> Self {
        Self::default()
    }
}

/// ARM7TDMI-specific errors
#[derive(Debug, thiserror::Error)]
pub enum Arm7tdmiError {
    /// JTAG probe error
    #[error("Probe error")]
    Probe(#[from] DebugProbeError),

    /// ARM-specific error
    #[error("ARM error")]
    Arm(#[from] ArmError),

    /// Timeout during operation
    #[error("Operation timed out")]
    Timeout,

    /// Invalid register number
    #[error("Invalid register number: {0}")]
    InvalidRegister(u8),

    /// Invalid hardware breakpoint/watchpoint unit index
    #[error("Invalid hardware breakpoint unit: {0}")]
    InvalidBreakpointUnit(usize),

    /// Core is not halted
    #[error("Core is not halted")]
    CoreNotHalted,

    /// Other error
    #[error("{0}")]
    Other(String),
}

impl From<Arm7tdmiError> for Error {
    fn from(err: Arm7tdmiError) -> Self {
        match err {
            Arm7tdmiError::Probe(e) => Error::Probe(e),
            Arm7tdmiError::Arm(e) => Error::Arm(e),
            Arm7tdmiError::Timeout => Error::Timeout,
            err => Error::Other(err.to_string()),
        }
    }
}

/// Communication interface for ARM7TDMI cores via EmbeddedICE
pub struct Arm7tdmiCommunicationInterface<'probe> {
    probe: JtagChain<'probe>,
    /// A reference (not an owned clone) into the persisted, session-lifetime
    /// [`Arm7tdmiDebugInterfaceState`] stored in [`crate::core::core_state::CombinedCoreState`]'s
    /// `JtagInterface::Arm7tdmi` slot. This *must* be a reference, not a clone: this whole struct
    /// is reconstructed fresh on every [`crate::Session::core`] call (see [`Self::new`]'s doc),
    /// and `pending_resume_pc` in particular is written by one call (e.g. a plain
    /// `write_core_reg(pc, ..)`, as `Session::prepare_running_on_ram` does) and consumed by a
    /// *later*, separate call's `resume()`/`run()` (e.g. `Session::resume_all_cores`, called
    /// independently afterward). Confirmed as a real, previously-unnoticed bug on real hardware:
    /// with an owned clone here (the original implementation), that later call's fresh interface
    /// instance always saw `pending_resume_pc: None` again - silently discarding the PC redirect
    /// and falling back to `resume()`'s "just continue from wherever it's halted" path instead,
    /// so a RAM-target `probe-rs run`/`download --start` on this architecture never actually
    /// jumped to the intended entry point at all. A hardware breakpoint armed at the entry point
    /// before resuming, in this exact two-call shape, never fired (confirmed: not even a single
    /// hit over a 5s window on a `clbss_l`-style loop body that would be executed hundreds of
    /// times if genuinely reached) - conclusive, not just theoretical.
    state: &'probe mut Arm7tdmiDebugInterfaceState,
    /// The JTAG instruction (IR) the probe's TAP currently holds, if known - `None` forces the
    /// next `scan_dr` to (re)select it. Mirrors OpenOCD's `arm_jtag_set_instr`, which checks
    /// `tap->cur_instr` and skips the IR-scan entirely when it already matches: unlike this
    /// crate's previous behaviour of reselecting IR before every single chain-1 clock (see
    /// `clock1`), real ARM7TDMI/EmbeddedICE hardware needs consecutive same-instruction scans
    /// to be plain DR shifts with IR held stationary, not a fresh IR-select each time - the
    /// per-clock IR churn was found to be why chain-1 always read back zero on real hardware.
    current_instruction: Option<JtagInstruction>,
    /// The scan chain most recently selected via SCAN_N, if known - see `current_instruction`;
    /// `select_scan_chain` skips reselecting when this already matches, matching OpenOCD's
    /// analogous `arm_jtag_scann` cache of `cur_scan_chain`.
    current_scan_chain: Option<ScanChain>,
}

impl<'probe> Arm7tdmiCommunicationInterface<'probe> {
    /// Create a new ARM7TDMI communication interface.
    ///
    /// Does *not* perform the real, physical TAP reset/IDCODE-read/watchpoint-clear
    /// initialization sequence - that's expensive and disruptive (a genuine TAP reset plus two
    /// EmbeddedICE register writes), and this constructor runs on *every* [`crate::Session::core`]
    /// call, not just the first one per session (this type is deliberately cheap/stateless to
    /// construct - the real, one-time session state lives in [`crate::architecture::arm7::Arm7tdmiState`],
    /// a level up). Call [`Self::init`] explicitly, gated on that persisted "already initialized"
    /// flag, instead of unconditionally here - see [`crate::architecture::arm7::Arm7tdmi::new`].
    pub fn new(probe: JtagChain<'probe>, state: &'probe mut Arm7tdmiDebugInterfaceState) -> Self {
        Self {
            probe,
            state,
            current_instruction: None,
            current_scan_chain: None,
        }
    }

    /// Reset the JTAG TAP and read back its IDCODE.
    ///
    /// A plain, non-destructive probe of the TAP - unlike [`Self::init`], this does not select
    /// the EmbeddedICE scan chain or touch the hardware breakpoint/watchpoint units, so it's
    /// safe to call just to identify what's on the other end of the JTAG chain (see the
    /// `probe-rs info` command).
    pub fn read_idcode(&mut self) -> Result<u32, Arm7tdmiError> {
        let mut batch = JtagBatch::new();
        self.probe.tap_reset(&mut batch);
        self.probe.run(batch)?;
        // TAP reset forces the TAP's IR/scan-chain-selection back to an implementation-defined
        // default, invalidating any cached assumption about what's currently selected.
        self.current_instruction = None;
        self.current_scan_chain = None;

        // IR and DR are selected/shifted together: see `scan_dr` below for why this can't be
        // split into a separate "select IR" step.
        let idcode = self.scan_dr(JtagInstruction::Idcode, 0, 32)?;
        Ok(idcode as u32)
    }

    /// Initialize the ARM7TDMI debug interface: reset the JTAG TAP, verify communication via
    /// IDCODE, select the EmbeddedICE scan chain, and defensively clear both hardware
    /// breakpoint/watchpoint units. Real, physical, disruptive work - call this once per
    /// session, not on every interface construction (see [`Self::new`]'s doc comment).
    pub(crate) fn init(&mut self) -> Result<(), Arm7tdmiError> {
        let idcode = self.read_idcode()?;
        tracing::debug!("ARM7TDMI IDCODE: 0x{:08X}", idcode);

        // Select scan chain 2 (EmbeddedICE)
        self.select_scan_chain(ScanChain::Chain2)?;

        // EmbeddedICE watchpoint/breakpoint units are separate hardware state that can
        // outlive a JTAG reconnect (they're not reset just by re-attaching). A stale enabled
        // watchpoint left armed from an earlier debug session could immediately re-halt the
        // core via breakpoint on the very next instruction fetch after RESTART, which would
        // show up as DBGACK set without SYSCOMP ever setting - clear both units defensively.
        for index in 0..Self::HW_BREAKPOINT_UNIT_COUNT {
            if let Err(e) = self.clear_hw_breakpoint(index) {
                tracing::warn!("failed to clear watchpoint unit {index}: {e:?}");
            }
        }

        tracing::debug!("ARM7TDMI communication interface initialized");
        Ok(())
    }

    /// Select the given JTAG instruction and shift `dr_bits` bits of `data` through the DR,
    /// returning the DR's previously captured content. Ends the shift in Run-Test/Idle
    /// (committing the DR via Update-DR).
    ///
    /// If `instruction` is already the TAP's currently-selected IR (per `current_instruction`),
    /// this skips the IR-select and shifts only the DR - matching OpenOCD's
    /// `arm_jtag_set_instr`, which checks `tap->cur_instr` the same way. This matters on real
    /// ARM7TDMI/EmbeddedICE hardware: reselecting IR before every chain-1 clock was found to
    /// prevent the core from ever actually single-stepping - chain-1 reads always came back zero
    /// until this was added.
    fn scan_dr(
        &mut self,
        instruction: JtagInstruction,
        data: u64,
        dr_bits: u32,
    ) -> Result<u64, Arm7tdmiError> {
        let dr_seq = BitSequence::from_u64(dr_bits as usize, data);

        let mut batch = JtagBatch::new();
        if self.current_instruction != Some(instruction) {
            let ir_seq = BitSequence::from_u64(IR_LEN, instruction as u64);
            self.probe.shift_ir(&mut batch, &ir_seq);
            self.current_instruction = Some(instruction);
        }
        let handle = self.probe.exchange_dr(&mut batch, &dr_seq);
        self.probe.run_test_idle(&mut batch, 0);
        let mut results = self.probe.run(batch)?;
        let captured = results
            .take(handle)
            .map_err(|_| Arm7tdmiError::Other("missing JTAG capture result".to_string()))?;

        Ok(captured.as_bits().load_le::<u64>())
    }

    /// Like [`Self::scan_dr`], but ends the shift in Pause-DR (via [`TapState::PauseDr`])
    /// instead of committing through Update-DR into Run-Test/Idle. Used only by
    /// [`Self::clock1`]/[`Self::clock1_no_capture`], not the chain-2 EmbeddedICE register
    /// access `scan_dr` is also used for (chain-1 specifically needs consecutive clocks to rest
    /// in Run-Test/Idle between each other, matching OpenOCD's ARM7TDMI clocking exactly - see
    /// `clock1`'s own doc comment).
    fn scan_dr_pause_idle(
        &mut self,
        instruction: JtagInstruction,
        data: u64,
        dr_bits: u32,
    ) -> Result<u64, Arm7tdmiError> {
        let dr_seq = BitSequence::from_u64(dr_bits as usize, data);

        let mut batch = JtagBatch::new();
        if self.current_instruction != Some(instruction) {
            // Cold-start IR reselect: land in Pause-DR right after the IR select itself, with no
            // real DR-side shift at all, before the real data shift that follows - matching
            // OpenOCD's `arm_jtag_set_instr(..., TAP_DRPAUSE)`.
            let ir_seq = BitSequence::from_u64(IR_LEN, instruction as u64);
            self.probe.shift_ir(&mut batch, &ir_seq);
            batch.enter(TapState::PauseDr);
            self.current_instruction = Some(instruction);
        }
        let handle = self.probe.exchange_dr(&mut batch, &dr_seq);
        batch.enter(TapState::PauseDr);
        self.probe.run_test_idle(&mut batch, 0);
        let mut results = self.probe.run(batch)?;
        let captured = results
            .take(handle)
            .map_err(|_| Arm7tdmiError::Other("missing JTAG capture result".to_string()))?;

        Ok(captured.as_bits().load_le::<u64>())
    }

    /// Like [`Self::scan_dr_pause_idle`], but never requests the shifted-out DR content back.
    fn scan_dr_pause_idle_no_capture(
        &mut self,
        instruction: JtagInstruction,
        data: u64,
        dr_bits: u32,
    ) -> Result<(), Arm7tdmiError> {
        let dr_seq = BitSequence::from_u64(dr_bits as usize, data);

        let mut batch = JtagBatch::new();
        if self.current_instruction != Some(instruction) {
            // See the matching branch in `scan_dr_pause_idle` above.
            let ir_seq = BitSequence::from_u64(IR_LEN, instruction as u64);
            self.probe.shift_ir(&mut batch, &ir_seq);
            batch.enter(TapState::PauseDr);
            self.current_instruction = Some(instruction);
        }
        let _ = self.probe.exchange_dr(&mut batch, &dr_seq);
        batch.enter(TapState::PauseDr);
        self.probe.run_test_idle(&mut batch, 0);
        self.probe.run(batch)?;
        Ok(())
    }

    /// Shift `dr_bits` bits of `data` through the DR of the currently selected IR (does not
    /// reselect the IR), returning the DR's previously captured content. Commits via Update-DR
    /// into Run-Test/Idle.
    fn shift_dr(&mut self, data: u64, dr_bits: u32) -> Result<u64, Arm7tdmiError> {
        let dr_seq = BitSequence::from_u64(dr_bits as usize, data);

        let mut batch = JtagBatch::new();
        let handle = self.probe.exchange_dr(&mut batch, &dr_seq);
        self.probe.run_test_idle(&mut batch, 0);
        let mut results = self.probe.run(batch)?;
        let captured = results
            .take(handle)
            .map_err(|_| Arm7tdmiError::Other("missing JTAG capture result".to_string()))?;

        Ok(captured.as_bits().load_le::<u64>())
    }

    /// Select a scan chain via the SCAN_N instruction.
    ///
    /// Skips reselecting if `chain` is already `current_scan_chain` - matching OpenOCD's
    /// `arm_jtag_scann`, which checks `cur_scan_chain` the same way. Selecting RESTART/other
    /// TAP-level instructions in between (as `system_speed_access` does) does not affect which
    /// chain SCAN_N last selected, only the IR value (tracked separately in
    /// `current_instruction`) - so this cache stays valid across those.
    fn select_scan_chain(&mut self, chain: ScanChain) -> Result<(), Arm7tdmiError> {
        if self.current_scan_chain == Some(chain) {
            return Ok(());
        }
        let chain_num = chain as u8;
        // Select SCAN_N and shift the chain number, both ending in Pause-DR rather than
        // committing through Update-DR/Update-IR - matching OpenOCD's
        // `arm_jtag_scann(..., TAP_DRPAUSE)`, which always parks in Pause-DR regardless of which
        // chain is being selected.
        let mut batch = JtagBatch::new();
        let ir_seq = BitSequence::from_u64(IR_LEN, JtagInstruction::ScanN as u64);
        self.probe.shift_ir(&mut batch, &ir_seq);
        batch.enter(TapState::PauseDr);
        let dr_seq = BitSequence::from_u64(4, chain_num as u64);
        let _ = self.probe.exchange_dr(&mut batch, &dr_seq);
        batch.enter(TapState::PauseDr);
        self.probe.run(batch)?;
        self.current_instruction = Some(JtagInstruction::ScanN);
        self.current_scan_chain = Some(chain);
        tracing::trace!("Selected scan chain {}", chain_num);
        Ok(())
    }

    /// Read an EmbeddedICE register (scan chain 2).
    ///
    /// JTAG's `Capture-DR` state always captures the register's content from *before* the
    /// current shift, so a single 38-bit scan (32-bit data, 5-bit address, 1-bit R/W) only
    /// latches the read address - it does not return data. A second scan (address/R/W
    /// unchanged) is required to actually capture the addressed register's value. See
    /// OpenOCD's `embeddedice_read_reg_w_check` for the equivalent two-scan technique.
    fn read_ice_register(&mut self, register: EmbeddedIceRegister) -> Result<u32, Arm7tdmiError> {
        self.select_scan_chain(ScanChain::Chain2)?;

        let address = register as u64;
        // R/W = 0 (bit 37) selects a read; bits [36:32] are the register address, bits
        // [31:0] are the data field (don't care for a read). Confirmed against OpenOCD's
        // `embeddedice_read_reg_w_check`, which documents bit 37 as "0/read".
        let access = address << 32;

        self.scan_dr(JtagInstruction::Intest, access, 38)?;
        let captured = self.shift_dr(access, 38)?;

        let value = (captured & 0xFFFF_FFFF) as u32;
        tracing::trace!("Read EmbeddedICE register {:?}: 0x{:08X}", register, value);
        Ok(value)
    }

    /// Write an EmbeddedICE register (scan chain 2).
    fn write_ice_register(
        &mut self,
        register: EmbeddedIceRegister,
        value: u32,
    ) -> Result<(), Arm7tdmiError> {
        self.select_scan_chain(ScanChain::Chain2)?;

        let address = register as u64;
        // R/W = 1 (bit 37) selects a write (see the note in `read_ice_register`).
        let access = (1u64 << 37) | (address << 32) | (value as u64);

        self.scan_dr(JtagInstruction::Intest, access, 38)?;
        // A write needs this follow-up scan (same access value) to actually commit, mirroring
        // the read path's "one behind" capture semantics - confirmed load-bearing on real
        // hardware, not just a defensive precaution.
        self.scan_dr(JtagInstruction::Intest, access, 38)?;

        tracing::trace!("Wrote EmbeddedICE register {:?}: 0x{:08X}", register, value);
        Ok(())
    }

    /// Read the Debug Status Register
    fn read_debug_status(&mut self) -> Result<u32, Arm7tdmiError> {
        self.read_ice_register(EmbeddedIceRegister::DebugStatus)
    }

    /// Write the Debug Control Register
    fn write_debug_control(&mut self, value: u32) -> Result<(), Arm7tdmiError> {
        self.write_ice_register(EmbeddedIceRegister::DebugControl, value)
    }

    /// Shift one value through scan chain 1 (the data/instruction bus plus the BREAKPT bit),
    /// pulsing one core clock, and return the bus content captured from the *previous* clock.
    ///
    /// Per ARM DDI 0029G section B.1.1 ("Scan chain 1"): "There are 33 bits in this scan
    /// chain, the order from serial data in to serial data out, is: 1. Data bus bits 0 to 31.
    /// 2. The BREAKPT bit, the first to be shifted out." This describes the register's
    /// physical layout (TDI end holds data bit 0, TDO end holds BREAKPT) - the chronological
    /// shift order is the *reverse* of that (confirmed against scan chain 2's analogous "R/W,
    /// address bits 4 to 0, then data value bits 31 to 0" layout, empirically validated on
    /// real hardware): BREAKPT first, then the 32 data bits MSB-first (bit 31 down to bit 0).
    ///
    /// RESOLVED: this scan chain used to always capture the same frozen value on real
    /// hardware, regardless of what was actually clocked out (confirmed via a byte-level MPSSE
    /// trace decode - see `jtag_decode.py` at the repo root, written for this investigation -
    /// that the *sent* bytes genuinely differed as expected; only the capture was stuck).
    /// Several real bugs were found and fixed along the way (IR/scan-chain reselection on
    /// every clock; capturing every clock instead of only the one whose result is used, see
    /// [`Self::clock1_no_capture`]; wrong instruction encodings - all worth keeping regardless,
    /// all independently confirmed correct against OpenOCD's source), but none of them alone
    /// explained the frozen capture. The actual final cause: this function's underlying DR
    /// shift went straight from Exit1-DR to Update-DR and directly into the next Select-DR-Scan
    /// (no Run-Test/Idle in between), while OpenOCD's ARM7TDMI chain-1 clocking rests in
    /// Run-Test/Idle between every consecutive clock - confirmed with a full op-by-op and then
    /// per-TCK-pulse state-path diff (via `jtag_decode.py`) between a probe-rs session and a
    /// real OpenOCD session on the same board. Fixed via [`Self::scan_dr_pause_idle`] (ends
    /// each shift in Pause-DR, then explicitly walks to Idle before the next one - see
    /// [`JtagAccess::move_to_idle`]). Confirmed fixed on real hardware: reads now exactly match
    /// an independent OpenOCD readback (including boot ROM content at `0x0`), and a RAM
    /// write+readback round-trips correctly, independently confirmed via OpenOCD's own `mdw`.
    fn clock1(&mut self, breakpt: bool, data: u32) -> Result<u32, Arm7tdmiError> {
        self.select_scan_chain(ScanChain::Chain1)?;

        let value = (breakpt as u64) | ((data.reverse_bits() as u64) << 1);
        let captured = self.scan_dr_pause_idle(JtagInstruction::Intest, value, 33)?;
        let captured_data = ((captured >> 1) & 0xFFFF_FFFF) as u32;
        Ok(captured_data.reverse_bits())
    }

    /// Like [`Self::clock1`], but never requests the shifted-out data bus content back - see
    /// [`JtagAccess::write_dr_no_capture`]. Use for fetch/decode/execute filler clocks and any
    /// other chain-1 clock whose result isn't read.
    fn clock1_no_capture(&mut self, breakpt: bool, data: u32) -> Result<(), Arm7tdmiError> {
        self.select_scan_chain(ScanChain::Chain1)?;

        let value = (breakpt as u64) | ((data.reverse_bits() as u64) << 1);
        self.scan_dr_pause_idle_no_capture(JtagInstruction::Intest, value, 33)?;
        Ok(())
    }

    /// Convert the core from Thumb state to ARM state, per OpenOCD's `arm7tdmi_change_to_arm`.
    ///
    /// Every [`arm7_instructions`] sequence in this file (`read_core_register`,
    /// `write_core_register`, `read_memory_32`, `write_memory_32`, ...) injects 32-bit
    /// ARM-encoded instructions via [`Self::clock1`]. If the core is halted in Thumb state
    /// (`debug_status::TBIT` set), the CPU is still decoding 16-bit Thumb instructions - feeding
    /// it 32-bit ARM opcodes would just be misdecoded as two garbage Thumb instructions. This
    /// must be called once, immediately after confirming a halt with `TBIT` set and before any
    /// other chain-1 sequence is attempted, using Thumb-encoded instructions instead (see
    /// [`thumb_instructions`]).
    ///
    /// Returns the original (pre-conversion) `r0` and `pc` values, exactly as OpenOCD's version
    /// does (it needs them to restore `r0` and report the correct halt PC).
    fn change_to_arm(&mut self) -> Result<(u32, u32), Arm7tdmiError> {
        use thumb_instructions::*;

        // Save r0 before clobbering it: fetch STR r0,[r0]; decode; execute(1); then a
        // capture-only clock reads back the value from execute(2) (see the module doc comment
        // on `arm7_instructions` for why capture lags one clock behind the shift that exposes
        // it - confirmed against OpenOCD's arm7tdmi_read_core_regs using the identical 4-clock
        // fetch+decode+execute+capture structure).
        self.clock1_no_capture(false, STR_R0_R0)?;
        self.clock1_no_capture(false, NOP)?;
        self.clock1_no_capture(false, NOP)?;
        let r0 = self.clock1(false, NOP)?;

        // r0 = pc (MOV r0, r15), then save it the same way. Unlike the r0 save above, this
        // needs one extra clock first to fetch MOV itself (STR is the very first instruction
        // in the r0 save above, whereas here MOV must be fetched before STR's own identical
        // fetch+decode+execute+capture 4-clock sequence can start) - confirmed by recounting
        // OpenOCD's arm7tdmi_change_to_arm literally: `clock_out(MOV); clock_out(STR);
        // clock_out(NOP); clock_out(NOP); clock_data_in(pc)` is 5 total operations, not 4.
        self.clock1_no_capture(false, MOV_R0_R15)?;
        self.clock1_no_capture(false, STR_R0_R0)?;
        self.clock1_no_capture(false, NOP)?;
        self.clock1_no_capture(false, NOP)?;
        let pc = self.clock1(false, NOP)?;

        // BX pc - the ARM7TDMI-S Technical Reference Manual's own documented sequence
        // (DDI 0234B, "Determining the core state") for switching Thumb to ARM state: reading
        // PC as an operand in Thumb state always yields instruction-address+4, which is
        // architecturally guaranteed halfword-aligned (bit 0 clear), so this unconditionally
        // switches to ARM state with no register load or memory access needed. Three clocks
        // (fetch, decode, execute), matching `execute_simple`'s pattern for a register-only
        // instruction with no result to capture.
        self.clock1_no_capture(false, BX_PC)?;
        self.clock1_no_capture(false, NOP)?;
        self.clock1_no_capture(false, NOP)?;

        // "MOV r0, r15 was the 4th instruction (+6)"; reading PC in Thumb state gives
        // address-of-instruction + 4. Matches OpenOCD's `*pc -= 0xa` exactly.
        let pc = pc.wrapping_sub(0xa);

        tracing::debug!("Converted core from Thumb to ARM state (r0=0x{r0:08X}, pc=0x{pc:08X})");
        Ok((r0, pc))
    }

    /// Run a register-only (non memory-accessing) instruction to completion: fetch, decode,
    /// execute. Safe to use at debug (TCK) speed since no external bus transaction occurs.
    fn execute_simple(&mut self, instruction: u32) -> Result<(), Arm7tdmiError> {
        self.clock1_no_capture(false, instruction)?;
        self.clock1_no_capture(false, arm7_instructions::NOP)?;
        self.clock1_no_capture(false, arm7_instructions::NOP)?;
        Ok(())
    }

    /// Run a single-word data-transfer instruction (`LDR`/`STR`) to completion, either
    /// capturing the data-bus value it produces (`inject: None`, used to read a register) or
    /// driving `inject` onto the data bus during its data-transfer cycle (used to load an
    /// arbitrary constant into a register without needing a literal pool).
    ///
    /// This only manipulates the core's internal register file / data bus latch via the scan
    /// chain - it does not perform a real, timing-correct external bus transaction. For
    /// accesses that must actually reach target memory (flash/RAM content), see
    /// [`Self::system_speed_access`] instead.
    fn execute_data_transfer(&mut self, instruction: u32) -> Result<u32, Arm7tdmiError> {
        // Confirmed against OpenOCD's arm7tdmi_read_core_regs: register values are read via
        // exactly fetch + 2 NOPs + one capture-only clock_data_in, i.e. 4 total 33-bit scans,
        // taking the 4th's capture as the answer. (Injecting an arbitrary literal into a
        // register is a *different* operation with different timing - see `load_immediate`,
        // which uses OpenOCD's actual `arm7tdmi_write_core_regs` mechanism instead of this.)
        self.clock1_no_capture(false, instruction)?; // fetch
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // decode
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // execute / address calculation
        self.clock1(false, arm7_instructions::NOP) // data-transfer cycle
    }

    /// Load an arbitrary 32-bit constant into `register` without touching real memory, by
    /// injecting it on the data bus during `LDMIA R0, {register}`'s data-transfer cycle. The
    /// base register's (R0's) value is irrelevant since the injected value overrides whatever
    /// address calculation the instruction would otherwise have used.
    ///
    /// Uses the LDM/STM multiple-register-transfer instruction, not a single-word `LDR`
    /// (`execute_data_transfer`'s capture path uses that, for reads) - confirmed against
    /// OpenOCD's `arm7tdmi_write_core_regs`, called with a single-bit mask by every real
    /// register-write path in OpenOCD (e.g. `arm7_9_write_memory`'s `reg[0] = address;
    /// write_core_regs(target, 0x1, reg)` to set up the address register): OpenOCD has no
    /// single-`LDR`-with-injected-data technique to load an arbitrary register at all. This is
    /// a real, independently-confirmed-correct fix (matches OpenOCD exactly), but on its own
    /// it did *not* resolve the frozen-capture symptom described on [`Self::clock1`] - that
    /// took a separate fix (only capturing on the clock whose result is used). Kept regardless,
    /// since it's still the right mechanism.
    fn load_immediate(&mut self, register: u8, value: u32) -> Result<(), Arm7tdmiError> {
        let instr = arm7_instructions::LOAD_MULTIPLE_R0 | (1u32 << register);
        self.clock1_no_capture(false, instr)?; // fetch LDMIA R0, {register}
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // decode
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // execute (1): address setup
        self.clock1_no_capture(false, value)?; // data-transfer cycle: inject `value`
        // Trailing clock for the injected value to commit to the register file - matches
        // OpenOCD's `arm7tdmi_write_core_regs`, which clocks one trailing NOP after the last
        // injected register value (`arm7tdmi_read_core_regs`, the capture-only counterpart,
        // has no equivalent - captures don't need one).
        self.clock1_no_capture(false, arm7_instructions::NOP)?;
        Ok(())
    }

    /// Write R15 (PC) via debug-speed `LDMIA R0, {R15}` - like [`Self::load_immediate`], but
    /// with three extra trailing NOP clocks. Matches OpenOCD's `arm7tdmi_write_pc` exactly,
    /// clock for clock: writing PC needs this longer settle time before the pipeline can be
    /// redirected via [`Self::branch_resume`], unlike any other single register.
    fn write_pc(&mut self, pc: u32) -> Result<(), Arm7tdmiError> {
        let instr = arm7_instructions::LOAD_MULTIPLE_R0 | (1u32 << 15);
        self.clock1_no_capture(false, instr)?; // fetch LDMIA R0, {R15}
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // decode
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // execute (1): address setup
        self.clock1_no_capture(false, pc)?; // execute (1): inject `pc`
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // execute (2)
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // execute (3)
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // execute (4)
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // execute (5)
        Ok(())
    }

    /// Queue a real ARM branch instruction back to the PC most recently written via
    /// [`Self::write_pc`], at debug speed, with `BREAKPT` armed on the preceding clock -
    /// matches OpenOCD's `arm7tdmi_branch_resume` exactly.
    ///
    /// Must be called with no other scan-chain-1 operation between it and the `write_pc` it
    /// follows: [`arm7_instructions::BRANCH_BACK_TO_PC`]'s offset is calibrated for that exact
    /// clock distance. This is why `resume()` always redoes `write_pc` immediately before
    /// calling this, rather than relying on an earlier, possibly-not-immediately-preceding
    /// `write_core_register(15, ..)` call.
    ///
    /// Confirmed necessary on real hardware: without this, `RESTART` only ran the core for a
    /// handful of instructions before it re-entered debug state, with PC nowhere near either
    /// the entry point or the intended completion breakpoint - a debug-speed `LDMIA`-injected
    /// PC value alone does not reliably redirect the fetch/decode pipeline on real ARM7TDMI
    /// silicon; only a genuine, pipeline-flushing branch instruction does.
    fn branch_resume(&mut self) -> Result<(), Arm7tdmiError> {
        self.clock1_no_capture(true, arm7_instructions::NOP)?;
        self.clock1_no_capture(false, arm7_instructions::BRANCH_BACK_TO_PC)?;
        Ok(())
    }

    /// Resume into `pc` in Thumb state, for a target that was halted in Thumb state - see
    /// [`Arm7tdmiDebugInterfaceState::pending_resume_thumb`]'s doc comment for why
    /// [`Self::write_pc`]/[`Self::branch_resume`] (a bare ARM `B`, no interworking capability at
    /// all) is not sufficient here.
    ///
    /// Ported clock-for-clock from OpenOCD's `arm7tdmi_branch_resume_thumb` (`arm7tdmi.c`) - the
    /// actual, hardware-proven reference mechanism for this exact problem on this exact debug
    /// architecture. An earlier attempt at this (synthesizing the T bit via a temporary SVC-mode
    /// SPSR write followed by `MOVS PC, LR`, an "exception return" idiom) is **not** how OpenOCD
    /// does it, and reliably corrupted execution on real hardware; OpenOCD's own `arm7_9_restore_context`
    /// explicitly masks the T bit *out* (`& ~0x20`) every time it writes CPSR, never using CPSR/
    /// SPSR MSR tricks to change it at all. The real, only-working mechanism is a genuine `BX`
    /// interworking branch: load the target `pc` with bit 0 forced set (the standard
    /// interworking-address convention - `BX` takes T from the target register's bit 0) into a
    /// scratch register, then execute `BX` on it - PC and CPSR.T update atomically as `BX`'s own
    /// documented architectural side effect. No CPSR/mode/flags/I-F capture or restore is needed
    /// at all: nothing in this whole sequence (or `change_to_arm`'s own earlier `BX pc`) ever
    /// touches anything but PC and T, so mode/flags/I/F are simply never disturbed from the real
    /// original halt's values.
    ///
    /// Uses R0 as the scratch/`BX` target register (clobbering it, like OpenOCD's own version -
    /// R0 is already known-clobbered-and-restorable at this point in `halt()`'s own Thumb
    /// handling), restoring its real value *after* the `BX`, via a Thumb-encoded PC-relative
    /// `LDR` (not another ARM-encoded op - the core is genuinely in Thumb state by then).
    fn branch_resume_thumb_aware(&mut self, pc: u32) -> Result<(), Arm7tdmiError> {
        let saved_r0 = self.read_core_register(0)?;

        // `LDMIA r0,{r0}` injecting `pc | 1` (bit 0 forced, the interworking-address convention)
        // - exactly `load_immediate`'s own mechanism (fetch, 2 NOPs, inject, trailing commit
        // clock - matches OpenOCD's own `LDMIA`+2-NOP+inject+NOP sequence here exactly).
        self.load_immediate(0, pc | 1)?;

        // `BX r0` - the real interworking branch; PC and CPSR.T update as its documented side
        // effect. Matches OpenOCD's own single, unaccompanied `clock_out` for this instruction -
        // deliberately not `execute_simple`'s fetch+2-NOP completion pattern (see
        // `branch_resume`'s own doc comment on why a debug-speed-injected PC write alone doesn't
        // reliably redirect the fetch/decode pipeline on this silicon - the settling below,
        // copied from OpenOCD exactly, is what actually does that job here).
        self.clock1_no_capture(false, arm7_instructions::BX_R0)?;

        // Let the transition settle, matching OpenOCD's own interleaved chain-2 status-read +
        // single-NOP-clock pattern exactly (its own comments mark the core as "now in Thumb
        // state" after this) - a chain-2 read doesn't care about ARM/Thumb decode state, so
        // these are safe regardless of exactly when T actually flips within this window.
        self.read_debug_status()?;
        self.clock1_no_capture(false, arm7_instructions::NOP)?;
        self.read_debug_status()?;
        self.clock1_no_capture(false, arm7_instructions::NOP)?;
        self.read_debug_status()?;

        // Restore r0's real value - via a Thumb-encoded PC-relative `LDR`, not another
        // ARM-encoded op, since the core is genuinely decoding Thumb from here on.
        self.clock1_no_capture(false, thumb_instructions::LDR_R0_PCREL)?;
        self.clock1_no_capture(false, thumb_instructions::NOP)?;
        self.clock1_no_capture(false, thumb_instructions::NOP)?;
        self.clock1_no_capture(false, saved_r0)?;
        self.clock1_no_capture(false, thumb_instructions::NOP)?;

        self.clock1_no_capture(false, thumb_instructions::NOP)?;
        self.clock1_no_capture(false, thumb_instructions::NOP)?;
        self.read_debug_status()?;

        // Final Thumb branch back to `pc` (already loaded into r0 above), BREAKPT armed on the
        // preceding clock - the Thumb-encoded equivalent of `branch_resume`'s own ARM `B`,
        // handing off to `resume()`'s trailing `RESTART`. Like `BRANCH_BACK_TO_PC`, this offset
        // is calibrated for this *exact* preceding clock sequence, not computed dynamically -
        // matches OpenOCD's own trailing `ARMV4_5_T_NOP` (bp=1) + `ARMV4_5_T_B(0x7f8)` exactly.
        self.clock1_no_capture(true, thumb_instructions::NOP)?;
        self.clock1_no_capture(false, thumb_instructions::BRANCH_BACK_TO_PC)?;
        Ok(())
    }

    /// Select `instruction` and go straight to Run-Test/Idle, with no DR-side shift at all -
    /// matching OpenOCD's `arm_jtag_set_instr(tap, N, NULL, TAP_IDLE)`.
    ///
    /// Used for RESTART, both here and in [`Self::resume`]: unlike a plain `scan_dr(Restart, 0,
    /// 1)`, which would also shift a 1-bit DR value that OpenOCD's reference implementation
    /// never does for RESTART.
    ///
    /// Includes one extra TCK pulse while already parked in Run-Test/Idle, matching a real
    /// OpenOCD trace of this exact sequence (`arm_jtag_set_instr(tap, RESTART, NULL, TAP_IDLE)`
    /// for ARM7TDMI's `resume()`): the captured TAP state path ends `...UpdIR->RTI->RTI`, i.e.
    /// one extra idle-state clock beyond the transition edge that lands in Idle. On this silicon
    /// that edge alone isn't sufficient to latch the debug logic's clock-domain-crossing "let the
    /// core run" signal - without this, `resume()`'s RESTART never actually took hold and the
    /// very next JTAG operation cut the core off before it ran meaningfully.
    fn select_ir_idle(&mut self, instruction: JtagInstruction) -> Result<(), Arm7tdmiError> {
        let mut batch = JtagBatch::new();
        let ir_seq = BitSequence::from_u64(IR_LEN, instruction as u64);
        self.probe.shift_ir(&mut batch, &ir_seq);
        self.probe.run_test_idle(&mut batch, 1);
        self.probe.run(batch)?;
        self.current_instruction = Some(instruction);
        Ok(())
    }

    /// Run a real, timing-correct system-speed access: the instruction currently queued to
    /// enter its data-transfer cycle is allowed to run against the *real* target bus (not the
    /// debug-speed scan chain), so wait states on flash/peripherals are handled correctly.
    ///
    /// Per ARM DDI 0029G section B.8.2 ("Determining system state"): BREAKPT (scan chain 1's
    /// 33rd bit) must be set on the clock for the instruction *prior to* the one that should
    /// run at system speed - i.e. on the execute/address-calculation clock, one before the
    /// pending data-transfer cycle (see [`Self::read_memory_32`]/[`Self::write_memory_32`]).
    /// Once that's scanned in, selecting RESTART here lets the core synchronize back to the
    /// system clock, execute that one data-transfer cycle at full bus speed (so wait states on
    /// flash/peripherals are handled correctly, unlike plain debug-speed clocking), then
    /// automatically re-enter debug state, signalled by DBGACK and SYSCOMP both being set in
    /// the Debug Status Register.
    ///
    /// Callers must clear `debug_control::STICKY_HALT` themselves *before* starting the
    /// chain-1 fetch/decode/execute clocking that precedes this call (see
    /// [`Self::read_memory_32`]/[`Self::write_memory_32`]), not from within this function:
    /// doing it here would reselect scan chain 2 (via `write_debug_control`) in between the
    /// chain-1 clocks and the RESTART that's supposed to complete them, losing the pending
    /// access. This mirrors OpenOCD's `arm7_9_write_memory`/`arm7_9_read_memory`, which clear
    /// `EICE_DBG_CONTROL_DBGACK` once, before the whole clock-out sequence, rather than
    /// per-call inside `arm7_9_execute_sys_speed`.
    fn system_speed_access(&mut self) -> Result<(), Arm7tdmiError> {
        self.select_ir_idle(JtagInstruction::Restart)?;

        let start = Instant::now();
        let timeout = Duration::from_secs(1);
        while start.elapsed() < timeout {
            let status = self.read_debug_status()?;
            if status & (debug_status::DBGACK | debug_status::SYSCOMP)
                == (debug_status::DBGACK | debug_status::SYSCOMP)
            {
                self.state.in_debug_state = true;
                return Ok(());
            }
            std::thread::sleep(Duration::from_micros(100));
        }

        Err(Arm7tdmiError::Timeout)
    }

    /// Read a core register (R0-R15, or 16 for CPSR).
    ///
    /// R0-R15 are implemented via a debug-speed `STMIA R0, {reg}` - matching OpenOCD's
    /// `arm7tdmi_read_core_regs`, which uses this multiple-register-transfer instruction even to
    /// read a single register, never a single-word `STR`. The store never needs to actually
    /// reach memory, since the value is captured directly off the internal data bus during the
    /// store's data-transfer cycle. Note this does perform a real store cycle using R0's
    /// *current* value as the target address - this is an inherent property of ARM7TDMI's debug
    /// architecture (there is no side-effect-free general-register-file access, unlike later Arm
    /// debug architectures), not a shortcut taken here; see ARM DDI 0029G Appendix B / OpenOCD's
    /// `arm7tdmi.c`. CPSR is different - see the `16 =>` arm below for why it deliberately does
    /// *not* reuse this same self-referential `STMIA R0, {R0}` capture trick.
    pub fn read_core_register(&mut self, register: u8) -> Result<u32, Arm7tdmiError> {
        self.ensure_halted()?;
        self.read_core_register_unchecked(register)
    }

    /// Same as [`Self::read_core_register`], without the "core is halted" precondition check -
    /// for callers that are themselves part of establishing that halt (currently only
    /// [`Self::enter_debug_state`]'s non-Thumb branch), where going through the checked version
    /// would recurse back into [`Self::halt`] via [`Self::ensure_halted`]'s recovery attempt.
    fn read_core_register_unchecked(&mut self, register: u8) -> Result<u32, Arm7tdmiError> {
        match register {
            0..=15 => {
                let instr = arm7_instructions::STORE_MULTIPLE_R0 | (1u32 << register);
                let value = self.execute_data_transfer(instr)?;
                if register == 15 {
                    // `STR PC, [Rn]` on ARMv4T stores PC + 12 (two pipeline stages ahead of
                    // the instruction actually executing), not the architectural PC value.
                    Ok(value.wrapping_sub(12))
                } else {
                    Ok(value)
                }
            }
            16 => {
                // Read CPSR via `MRS R0, CPSR` immediately followed (pipelined, no filler NOPs
                // in between) by `STR R0, [R15]` to capture it - matching OpenOCD's
                // `arm7tdmi_read_xpsr` clock-for-clock instead of this crate's usual
                // self-referential `STMIA R0, {R0}` capture trick (fine for a normal 0-15
                // register read, where R0 holds plausible program state, but CPSR's bit pattern
                // is essentially never 4-byte aligned, and R0 holds CPSR *itself* by the time
                // the capture instruction executes - using it as its own store's memory base
                // looked like a plausible reason OpenOCD avoids it here).
                //
                // RESOLVED (2026-09-08): the garbage/stale-R0 readings previously described here
                // were never caused by this capture sequence at all - see `enter_debug_state`'s
                // doc comment for the actual root cause (a Thumb-halted core never being
                // converted to ARM state before this file's ARM-encoded chain-1 injections, for
                // any halt reached via a hardware breakpoint/watchpoint). With that fixed, this
                // sequence now reads back correctly.
                self.clock1_no_capture(false, arm7_instructions::MRS_R0_CPSR)?; // fetch MRS
                self.clock1_no_capture(false, arm7_instructions::STR_R0_R15)?; // fetch STR (MRS decode)
                self.clock1_no_capture(false, arm7_instructions::NOP)?; // MRS execute, STR decode
                self.clock1_no_capture(false, arm7_instructions::NOP)?; // STR execute (1st cycle)
                let cpsr = self.clock1(false, arm7_instructions::NOP)?; // STR execute (2nd cycle) / capture

                // This capture reflects the core's *real, current* CPSR - which genuinely has
                // T=0 (ARM) right now if `enter_debug_state` converted it from an originally
                // Thumb-mode halt (`change_to_arm`'s `BX pc` is a real architectural state
                // change, not a debug-side pretense). A caller wants the CPSR the target will
                // actually resume with, though, which had T=1 - restore it from
                // `pending_resume_thumb`, the same flag `enter_debug_state`/`resume()` already
                // use to remember this exact fact. Confirmed against real OpenOCD ground truth at
                // an identical Thumb-mode halt point: this file's read of `0x6000001f` plus this
                // restore now matches OpenOCD's own `0x6000003f` exactly (bit 5 is CPSR's T bit).
                if self.state.pending_resume_thumb {
                    Ok(cpsr | (1 << 5))
                } else {
                    Ok(cpsr)
                }
            }
            _ => Err(Arm7tdmiError::InvalidRegister(register)),
        }
    }

    /// Read several general-purpose registers (R0-R15) in a single debug-speed `STMIA`, instead
    /// of one per register.
    ///
    /// Matches OpenOCD's `arm7tdmi_read_core_regs` exactly: fetch `STMIA R0, {mask}` + two NOPs
    /// (decode, execute/address-setup), then one capture clock per set bit in `mask`, in
    /// ascending register-number order (the order a real multi-register `STM` transfers them in,
    /// regardless of bit order) - see [`Self::read_core_register`] for the single-register
    /// version this generalizes and its own doc comment for why a register read has a *real*,
    /// physical side effect on this silicon (each one genuinely advances the core's pipeline by
    /// a few instructions). Reading N registers this way pays that fetch/decode/execute overhead
    /// once, not N times - see [`crate::architecture::arm7`]'s module doc and the
    /// `step_wreg_pc_anomaly_explained` memory entry for why that matters in practice (repeated
    /// single-register reads during interactive stepping/inspection can walk a short routine
    /// clean off its own end).
    ///
    /// `mask`'s bit 15 (R15/PC) gets the same `- 12` adjustment as
    /// [`Self::read_core_register`]'s register-15 case. CPSR (register 16) is not part of this
    /// transfer at all (it isn't in the R0-R15 register file) - callers wanting it still need a
    /// separate [`Self::read_core_register`]`(16, ..)` call.
    ///
    /// A single `STMIA`-based capture sequence only reliably captures the first
    /// [`Self::MAX_REGISTERS_PER_TRANSFER`] registers - confirmed on real hardware, precisely
    /// bisected: requesting 4 registers in one transfer reads back 4 genuinely distinct values,
    /// but requesting 5 makes the 4th and 5th data-transfer clocks return the *same* (stale)
    /// value instead of two real ones, and every clock past the 4th continues repeating it. This
    /// held regardless of which specific registers were requested (shifting the whole request by
    /// one register number shifted where the stale run started by the same amount), ruling out
    /// any specific register (e.g. R0, also `STMIA`'s own base register) as the cause - it's
    /// purely about how many data-transfer clocks are chained back-to-back in one sequence, not
    /// which registers. Root cause not identified further (a JTAG-adapter-side buffering limit
    /// was checked and ruled out - this probe's 4096-byte command buffer is far larger than
    /// needed - so this is presumably a genuine ARM7TDMI/EmbeddedICE silicon limit on sustained
    /// `STMIA` data-transfer cycles at debug speed). Worked around here by chunking any request
    /// into groups of at most [`Self::MAX_REGISTERS_PER_TRANSFER`] registers, each getting its
    /// own fresh fetch/decode/execute/capture sequence, rather than one unbounded transfer -
    /// preserves the single-transfer fast path for anything at or under the limit (e.g. the
    /// already-validated 4-register case) while fixing larger requests.
    pub fn read_core_registers(&mut self, mask: u16) -> Result<Vec<(u8, u32)>, Arm7tdmiError> {
        self.ensure_halted()?;

        let mut results = Vec::with_capacity(mask.count_ones() as usize);
        let mut remaining = mask;
        while remaining != 0 {
            let mut chunk_mask: u16 = 0;
            let mut chunk_count = 0u8;
            for register in 0..=15u8 {
                if remaining & (1 << register) == 0 {
                    continue;
                }
                chunk_mask |= 1 << register;
                remaining &= !(1 << register);
                chunk_count += 1;
                if chunk_count == Self::MAX_REGISTERS_PER_TRANSFER {
                    break;
                }
            }

            let instr = arm7_instructions::STORE_MULTIPLE_R0 | chunk_mask as u32;
            self.clock1_no_capture(false, instr)?; // fetch
            self.clock1_no_capture(false, arm7_instructions::NOP)?; // decode
            self.clock1_no_capture(false, arm7_instructions::NOP)?; // execute / address calculation

            for register in 0..=15u8 {
                if chunk_mask & (1 << register) == 0 {
                    continue;
                }
                let value = self.clock1(false, arm7_instructions::NOP)?; // data-transfer cycle
                let value = if register == 15 {
                    value.wrapping_sub(12)
                } else {
                    value
                };
                results.push((register, value));
            }
        }
        Ok(results)
    }

    /// The most registers [`Self::read_core_registers`] will fold into a single `STMIA`
    /// data-transfer sequence before starting a fresh one - see that method's doc comment for
    /// how this limit was found.
    const MAX_REGISTERS_PER_TRANSFER: u8 = 4;

    /// Read the current mode's banked SPSR via `MRS R0, SPSR` + `STR R0, [R15]`, clock-for-clock
    /// identical to [`Self::read_core_register`]'s CPSR (register 16) case, just with
    /// [`arm7_instructions::MRS_R0_SPSR`] instead of `MRS_R0_CPSR` - see that case's doc comment
    /// for why this specific capture sequence (not the usual self-referential `STMIA R0, {R0}`
    /// trick) is used. Unlike that CPSR read, this returns the *raw* captured value with no
    /// `pending_resume_thumb`-based T-bit synthesis - SPSR's T bit is real, hardware-captured
    /// data (the interrupted code's actual state at exception entry), not something this
    /// interface needs to reconstruct.
    fn read_spsr(&mut self) -> Result<u32, Arm7tdmiError> {
        self.clock1_no_capture(false, arm7_instructions::MRS_R0_SPSR)?; // fetch MRS
        self.clock1_no_capture(false, arm7_instructions::STR_R0_R15)?; // fetch STR (MRS decode)
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // MRS execute, STR decode
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // STR execute (1st cycle)
        self.clock1(false, arm7_instructions::NOP) // STR execute (2nd cycle) / capture
    }

    /// Return from the current exception, exactly as a normal (un-debugged) `SVC`/`UNDEF`
    /// handler would: sets PC to LR and restores CPSR (mode/flags/T) from the current mode's
    /// banked SPSR - the exact state the interrupted code had.
    ///
    /// Used to resume from a semihosting call intercepted via SVC vector catch (see
    /// [`crate::architecture::arm7::Arm7tdmi::run`]).
    ///
    /// BUG FOUND (2026-09-08) and fixed: this used to do this via a single, real `MOVS PC, LR`
    /// instruction (`arm7_instructions::MOVS_PC_LR`) injected and executed on the target -
    /// exactly the "exception return" idiom
    /// [`Self::branch_resume_thumb_aware`]'s own doc comment already documents as *not* how
    /// OpenOCD does a Thumb-state resume and as reliably corrupting execution on this real
    /// hardware (an MSR/CPSR-restore-driven T-bit change is architecturally unpredictable -
    /// only `BX` reliably changes it). That lesson had been applied everywhere else in this
    /// file's resume machinery except here. Compounding this, the real `MOVS PC, LR` also
    /// silently left [`Arm7tdmiDebugInterfaceState::pending_resume_pc`]/`pending_resume_thumb`
    /// exactly as they were *before* the call (both bookkeeping fields specific to *this*
    /// file's own explicit-PC-write tracking, which a raw injected instruction execution never
    /// touches) - so the very next `resume()` call in `run()` would redirect PC right back to
    /// the *stale*, pre-exception-return cached value (the SVC vector's own address) instead of
    /// wherever `MOVS PC, LR` had just, correctly, really branched to - undoing the return
    /// entirely and re-entering the SVC vector in a tight, silent, JTAG-invisible loop (matching
    /// the observed symptom exactly: `embedded-test` hanging indefinitely, with no further
    /// tracing at all, right after the first successful semihosting dispatch).
    ///
    /// Fixed by not executing a real exception-return instruction at all: read SPSR (the exact
    /// CPSR the interrupted code had - real, hardware-captured data, safe to read while still
    /// genuinely in ARM state) and LR (the resume address) first, then let the already-proven,
    /// `BX`-based [`Self::resume`]/[`Self::branch_resume_thumb_aware`] machinery perform the
    /// actual T-bit/PC transition - by caching them into the exact same
    /// `pending_resume_pc`/`pending_resume_thumb` fields `resume()` already knows how to consume
    /// correctly. Mode/interrupt-mask/flag bits (everything in SPSR except T) are restored via
    /// the existing MSR-based register-16 write, with T forced to the real current value (0 -
    /// this function only ever runs immediately after a fresh SVC halt, genuinely in ARM state)
    /// so that write never touches T via MSR at all.
    pub(crate) fn return_from_exception(&mut self) -> Result<(), Arm7tdmiError> {
        self.ensure_halted()?;

        // `read_spsr`'s `MRS R0, SPSR` genuinely, permanently overwrites the target's real R0 -
        // it's a real instruction execution, not a side-effect-free debug read (see its own doc
        // comment). This matters here specifically because a semihosting call's return value
        // (e.g. `write_status`, called by the RPC layer *before* this function, as part of
        // servicing the same halt) is conventionally placed in R0 for the target to read once it
        // resumes - if `read_spsr` clobbers it first, the target sees SPSR's raw bit pattern as
        // its syscall's "return value" instead of the real one, almost always a nonzero value on
        // this specific SVC-vector-catch path (SPSR here is the *caller's* CPSR, essentially
        // never zero) that the target's semihosting client library reads as a hard failure -
        // confirmed hardware root cause of a genuine, reproducible panic in `embedded-test`'s own
        // `args()` call chain, previously misattributed to a target-side "corrupted stack word"/
        // "spurious IRQ" bug in a much longer prior investigation. Save/restore R0 around the
        // clobbering read so any value a caller already placed there survives this call.
        let saved_r0 = self.read_core_register(0)?;
        let spsr = self.read_spsr()?;
        let lr = self.read_core_register(14)?;

        const CPSR_TBIT: u32 = 1 << 5;
        self.write_core_register_unchecked(16, spsr & !CPSR_TBIT)?;
        self.write_core_register_unchecked(0, saved_r0)?;

        self.cache_resume_pc(lr);
        self.state.pending_resume_thumb = spsr & CPSR_TBIT != 0;
        Ok(())
    }

    /// Write a core register (R0-R15, or 16 for CPSR).
    ///
    /// R0-R14 are written by injecting `value` via [`Self::load_immediate`]. R15 (PC) instead
    /// goes through [`Self::write_pc`] (a longer, PC-specific clock sequence) and is cached in
    /// [`Arm7tdmiDebugInterfaceState::pending_resume_pc`] for `resume()` to redo, immediately
    /// followed by a real branch instruction, right before the core is actually let run - see
    /// the note on `resume()` for why a plain debug-speed PC write isn't enough on its own.
    ///
    /// CPSR is written a byte at a time via four `MSR CPSR_<field>, #imm` instructions (the
    /// only way to place an arbitrary 32-bit value into CPSR, since MSR's immediate operand is
    /// an 8-bit value with a rotation). Note that if the written value changes the Thumb (T)
    /// bit, the core will start decoding the following JTAG-injected words as 16-bit Thumb
    /// instructions instead of 32-bit ARM ones, which would break any further access through
    /// this interface - this is an inherent hazard of writing CPSR this way, not handled here.
    pub fn write_core_register(&mut self, register: u8, value: u32) -> Result<(), Arm7tdmiError> {
        self.ensure_halted()?;
        self.write_core_register_unchecked(register, value)
    }

    /// Like [`Self::write_core_register`], but without the `is_halted()` precondition check -
    /// for callers (currently only [`Self::enter_debug_state`]) that just confirmed DBGACK
    /// themselves, moments earlier in the same call chain, and want to avoid adding more chain-2
    /// `DebugStatus` round-trips to an already-dense burst of them right after a fresh halt - a
    /// window this whole backend has repeatedly found less reliable than isolated, well-spaced
    /// reads (see `probe_rs_arm7_chain1_bug` memory).
    fn write_core_register_unchecked(
        &mut self,
        register: u8,
        value: u32,
    ) -> Result<(), Arm7tdmiError> {
        match register {
            15 => {
                self.write_pc(value)?;
                self.state.pending_resume_pc = Some(value);
                // An explicit PC write redirects to a specific target the caller chose (e.g.
                // `prepare_running_on_ram`'s boot-entry redirect) - every such real caller in
                // this codebase targets ARM-mode code, so a stale `pending_resume_thumb` left
                // over from whatever *arbitrary* state the core happened to be halted in before
                // this call (Thumb or not - irrelevant now, this is a fresh, unrelated target)
                // must not leak into `resume()`'s choice of resume mechanism. `halt()`'s own
                // Thumb-branch calls this same function to restore the *original* pc after
                // `change_to_arm`'s clobbering - deliberately sets its own
                // `pending_resume_thumb = true` *after* that call, not before, so this reset
                // doesn't clobber it right back.
                self.state.pending_resume_thumb = false;
                Ok(())
            }
            0..=14 => {
                self.load_immediate(register, value)?;
                Ok(())
            }
            16 => {
                // (field_mask, rotate_imm, byte)
                let fields = [
                    (0b0001u32, 0u32, value & 0xFF),
                    (0b0010, 12, (value >> 8) & 0xFF),
                    (0b0100, 8, (value >> 16) & 0xFF),
                    (0b1000, 4, (value >> 24) & 0xFF),
                ];
                for (mask, rotate, imm8) in fields {
                    let instr =
                        arm7_instructions::MSR_CPSR_IMM | (mask << 16) | (rotate << 8) | imm8;
                    self.execute_simple(instr)?;
                }
                Ok(())
            }
            _ => Err(Arm7tdmiError::InvalidRegister(register)),
        }
    }

    /// Read memory at the given address (must be word-aligned).
    ///
    /// Unlike register access, this must reach the real target bus (flash/RAM may have wait
    /// states), so the actual `LDR` runs via [`Self::system_speed_access`] rather than the
    /// debug-speed pipeline clocking used for register transfers.
    pub fn read_memory_32(&mut self, address: u32) -> Result<u32, Arm7tdmiError> {
        self.ensure_halted()?;

        // Clear `STICKY_HALT` for the duration of the system-speed access below (see the note
        // on `system_speed_access`), before starting the chain-1 clock sequence it completes.
        self.write_debug_control(debug_control::INTDIS)?;

        // R0 = address
        self.load_immediate(0, address)?;
        // (Removed: a diagnostic `read_core_register(0/1/2)` probe used to sit here, each call
        // round-tripping through scan chain 2 via its own `is_halted()`. Tested and ruled out
        // as the cause of the "every capture reads back the same frozen value" symptom - real
        // reads at different addresses still return identically 0xE59FF860 without it - but
        // it's dead diagnostic weight either way, so left removed.)

        // Queue `LDMIA R0!, {R1}` for system-speed execution, matching OpenOCD's
        // `arm7tdmi_load_word_regs` (`ARMV4_5_LDMIA(0, mask, 0, 1)`) exactly - this previously
        // queued a single-word `LDR R1, [R0]` instead, despite this comment already (correctly)
        // describing it as "load-multiple": OpenOCD never uses a single-word LDR/STR for a
        // system-speed access, only the write-back LDM/STM form. This was a real bug - the
        // single-word LDR queued here never actually landed the requested address's content.
        // BREAKPT goes on a NOP clocked *before* the target instruction's own fetch - not
        // during the target instruction's own execute/address-calc stage, as this used to do.
        // The instruction is then only *fetched* (bp=false) at debug speed; RESTART lets the
        // core run its decode/execute/data-transfer stages automatically at system speed.
        self.clock1_no_capture(false, arm7_instructions::NOP)?;
        self.clock1_no_capture(true, arm7_instructions::NOP)?;
        let instr = arm7_instructions::LOAD_MULTIPLE_R0_WRITEBACK | (1 << 1);
        self.clock1_no_capture(false, instr)?; // fetch LDMIA R0!, {R1}

        // The re-latch below must happen even when the access *fails*. `system_speed_access`
        // issues RESTART before it starts polling, so by the time it can time out the core has
        // already been released; returning early via `?` with STICKY_HALT still clear would
        // leave the debug logic half-configured for whatever runs next. Latent-invariant fix
        // only: this was investigated as a candidate cause of the "Core is not halted on every
        // subsequent attach" wedge seen after `erase`/`debug --launch` and hardware-tested to
        // NOT be it (the wedge still reproduces with this in place) - that turned out to be the
        // core being resumed into post-flash-algorithm garbage at teardown, recoverable with a
        // target reset. Kept because restoring the latch on the error path is correct either way.
        let access = self.system_speed_access();
        // Confirmed against a real OpenOCD trace on this exact board (`embeddedice_write_reg():
        // 0: 0x00000005`): STICKY_HALT|INTDIS, not DBGRQ|INTDIS.
        self.write_debug_control(debug_control::STICKY_HALT | debug_control::INTDIS)?;
        access?;

        self.read_core_register(1)
    }

    /// Write memory at the given address (must be word-aligned).
    pub fn write_memory_32(&mut self, address: u32, value: u32) -> Result<(), Arm7tdmiError> {
        self.ensure_halted()?;

        // Clear `STICKY_HALT` for the duration of the system-speed access below (see the note
        // on `system_speed_access`), before starting the chain-1 clock sequence it completes.
        self.write_debug_control(debug_control::INTDIS)?;

        // R0 = address, R1 = value
        self.load_immediate(0, address)?;
        self.load_immediate(1, value)?;

        // Queue `STMIA R0!, {R1}` for system-speed execution - see the note in `read_memory_32`
        // (matches OpenOCD's `arm7tdmi_store_word_regs`, the write-back STM form; this
        // previously queued a single-word `STR R1, [R0]`, which never actually landed the
        // write).
        self.clock1_no_capture(false, arm7_instructions::NOP)?;
        self.clock1_no_capture(true, arm7_instructions::NOP)?;
        let instr = arm7_instructions::STORE_MULTIPLE_R0_WRITEBACK | (1 << 1);
        self.clock1_no_capture(false, instr)?; // fetch STMIA R0!, {R1}

        // Re-latch debug state even if the access failed - see `read_memory_32`.
        let access = self.system_speed_access();
        self.write_debug_control(debug_control::STICKY_HALT | debug_control::INTDIS)?;
        access?;
        Ok(())
    }

    /// Write consecutive words starting at `address`, as a single burst.
    ///
    /// Unlike calling [`Self::write_memory_32`] once per word, this clears `STICKY_HALT` only
    /// once for the whole burst (not once per word) and reloads R0 with the next address only
    /// once at the start, relying on the `STMIA R0!, {R1}` write-back to advance it - exactly
    /// matching OpenOCD's `arm7_9_write_memory` (which explicitly clears `EICE_DBG_CONTROL_DBGACK`
    /// - the same physical bit this interface calls `STICKY_HALT` - once before its whole write
    /// loop and only re-sets it once after, with the comment "Clear DBGACK, to make sure memory
    /// fetches work as expected"; it also batches up to 14 registers per system-speed access,
    /// which this does not replicate, but the single-register-per-access form still avoids the
    /// bug below).
    ///
    /// Confirmed on real hardware to fix a real bug: calling `write_memory_32` in a plain loop -
    /// which toggles `STICKY_HALT` off then back on for *every single word* - reliably,
    /// deterministically drifts the core's real, architectural PC by tens of KB over a 64-word
    /// (256-byte) burst, with the core ending up executing unrelated code for real, even though
    /// each individual `write_memory_32` call's own `is_halted()` polling reports success
    /// throughout. Verified via an independent OpenOCD session performing the identical 64-word
    /// burst against the same board: PC and CPSR read back bit-for-bit identical before and
    /// after (no drift at all). This function's burst form (clear once, write N words, set once)
    /// was verified the same way and showed no drift either - narrowing the bug specifically to
    /// the per-word `STICKY_HALT` clear/set cycle, not anything else about system-speed access.
    pub fn write_memory_32_bulk(
        &mut self,
        address: u32,
        values: &[u32],
    ) -> Result<(), Arm7tdmiError> {
        if values.is_empty() {
            return Ok(());
        }

        self.ensure_halted()?;

        self.write_debug_control(debug_control::INTDIS)?;

        self.load_immediate(0, address)?;

        // A failure part-way through the burst must still fall through to the re-latch below,
        // rather than returning early with STICKY_HALT clear - see `read_memory_32`.
        let mut access = Ok(());
        for &value in values {
            self.load_immediate(1, value)?;

            self.clock1_no_capture(false, arm7_instructions::NOP)?;
            self.clock1_no_capture(true, arm7_instructions::NOP)?;
            let instr = arm7_instructions::STORE_MULTIPLE_R0_WRITEBACK | (1 << 1);
            self.clock1_no_capture(false, instr)?; // fetch STMIA R0!, {R1}

            access = self.system_speed_access();
            if access.is_err() {
                break;
            }
        }

        self.write_debug_control(debug_control::STICKY_HALT | debug_control::INTDIS)?;
        access?;
        Ok(())
    }

    /// Read consecutive words starting at `address`, as a burst.
    ///
    /// BUG FOUND (2026-09-10): there was no bulk memory-read primitive on this backend at all -
    /// even the generic `read_32` (`mod.rs`) looped calling the single-word [`Self::read_memory_32`]
    /// once per word, the exact same per-word `STICKY_HALT` clear/set drift class already found
    /// and fixed for writes (see [`Self::write_memory_32_bulk`]'s doc comment) - just never
    /// applied to reads. Fixed the same way, reusing an already-proven building block: queues a
    /// multi-register `LDMIA R0!, {R1..R(chunk)}` (chunk size bounded by
    /// [`Self::MAX_REGISTERS_PER_TRANSFER`] = 4 - the same hard per-STMIA/LDMIA-capture register
    /// limit already found and worked around in [`Self::read_core_registers`]) as ONE
    /// system-speed access per up-to-4 words (one `STICKY_HALT` toggle per chunk, not per word),
    /// then pulls the captured registers back out via [`Self::read_core_registers`]'s own
    /// already-hardware-verified batched chain-1 capture - not a new, unverified mechanism.
    pub fn read_memory_32_bulk(
        &mut self,
        address: u32,
        count: usize,
    ) -> Result<Vec<u32>, Arm7tdmiError> {
        if count == 0 {
            return Ok(Vec::new());
        }

        self.ensure_halted()?;

        let mut results = Vec::with_capacity(count);
        let mut remaining = count;
        let mut addr = address;

        loop {
            let chunk = remaining.min(Self::MAX_REGISTERS_PER_TRANSFER as usize);
            // Registers R1..=R(chunk) - mirrors `read_core_registers`'s own per-chunk mask
            // construction, just starting at R1 (R0 is the LDMIA base register here).
            let reg_mask: u32 = (1u32 << (chunk + 1)) - 2;

            self.write_debug_control(debug_control::INTDIS)?;
            self.load_immediate(0, addr)?;

            self.clock1_no_capture(false, arm7_instructions::NOP)?;
            self.clock1_no_capture(true, arm7_instructions::NOP)?;
            let instr = arm7_instructions::LOAD_MULTIPLE_R0_WRITEBACK | reg_mask;
            self.clock1_no_capture(false, instr)?; // fetch LDMIA R0!, {R1..R(chunk)}

            // Re-latch debug state even if the access failed - see `read_memory_32`.
            let access = self.system_speed_access();
            self.write_debug_control(debug_control::STICKY_HALT | debug_control::INTDIS)?;
            access?;

            // Pull R1..R(chunk) back out via the already-proven batched capture path - not a
            // new, separately-verified read mechanism. `read_core_registers` returns its results
            // in ascending register order for a single chunk (guaranteed here: `reg_mask` never
            // spans more than `MAX_REGISTERS_PER_TRANSFER` bits), so no separate sort is needed.
            let captured = self.read_core_registers(reg_mask as u16)?;
            for (_, value) in captured {
                results.push(value);
            }

            addr = addr.wrapping_add((chunk as u32) * 4);
            remaining -= chunk;
            if remaining == 0 {
                break;
            }
        }

        Ok(results)
    }

    /// Halt the core by setting DBGRQ in the Debug Control Register
    pub fn halt(&mut self) -> Result<(), Arm7tdmiError> {
        tracing::info!("Halting ARM7TDMI core");

        // Set DBGRQ bit in Debug Control Register
        self.write_debug_control(debug_control::DBGRQ)?;

        // Wait for core to enter debug state (DBGACK)
        let start = std::time::Instant::now();
        // A direct trace analysis (RUST_LOG=probe_rs::probe::ftdi=trace, jtag_decode.py ground
        // truth) of a real, reproducible failure on this backend showed this poll loop running
        // continuously for the *entire* previous 1s budget (~318 back-to-back JTAG round trips,
        // ~3.1ms each after removing the old 10ms inter-poll sleep) without DBGACK ever being
        // observed set, versus ~172 iterations before a typical success - i.e. DBGRQ sometimes
        // genuinely needs more than 1s of continuous polling to take effect on this silicon, not
        // that the protocol itself is wrong. Extending the budget is a safe, backward-compatible
        // change (a successful halt still returns as soon as DBGACK is seen, same as before).
        let timeout = std::time::Duration::from_secs(3);

        while start.elapsed() < timeout {
            let status = self.read_debug_status()?;
            if status & debug_status::DBGACK != 0 {
                self.state.in_debug_state = true;
                // "Debug entry": latch the halt via `STICKY_HALT` and release DBGRQ, so the
                // core stays halted without DBGRQ needing to remain asserted. Confirmed against
                // a real OpenOCD trace on this exact board (`embeddedice_write_reg(): 0:
                // 0x00000005`).
                self.write_debug_control(debug_control::STICKY_HALT | debug_control::INTDIS)?;
                // OpenOCD's `arm7_9_debug_entry` always follows this same write with a call to
                // `arm7_9_clear_halt`, which - specifically for a plain DBGRQ-triggered halt (as
                // opposed to a reset-triggered one, which `reset_and_halt` handles separately and
                // does not need this) - issues a SECOND, distinct EmbeddedICE register commit of
                // the exact same bit pattern (`DBGACK=1`/`DBGRQ=0`/`INTDIS=1`), not just a
                // software no-op: `arm7_9_clear_halt`'s first branch
                // (`!debug_entry_from_reset && use_dbgrq`) unconditionally re-writes the debug
                // control register even though the cached value hasn't changed since the write
                // just above. This is the one concrete, structural difference found between
                // OpenOCD's (reliable, 15/15 verified on this exact board - see
                // `probe_rs_arm7_chain1_bug` memory) plain-DBGRQ halt sequence and this
                // function's own, which every one of this session's lockups traced back to
                // (attach-while-running, i.e. exactly this path - `reset_and_halt`, used by every
                // proven-reliable flashing operation, never goes through this and is untouched).
                self.write_debug_control(debug_control::STICKY_HALT | debug_control::INTDIS)?;
                tracing::debug!("Core halted successfully");
                self.enter_debug_state(status, true)?;
                return Ok(());
            }
            // OpenOCD's own DBGACK poll loop (`target_wait_state`, calling `target_poll` ->
            // `arm7_9_poll`) has no sleep at all between JTAG reads - it polls back-to-back as
            // fast as the JTAG transfer itself takes. This 10ms fixed sleep has no equivalent
            // there; removing it to match, as part of narrowing the remaining, still-
            // intermittent lockup this session's `arm7_9_clear_halt` fix didn't fully close (see
            // `probe_rs_arm7_chain1_bug` memory).
        }

        Err(Arm7tdmiError::Timeout)
    }

    /// Finish entering debug state after a halt has just been durably latched (`STICKY_HALT`
    /// set), regardless of how the halt was reached - a DBGRQ-based [`Self::halt`] or a
    /// watchpoint match via [`Self::latch_watchpoint_halt`] both need this exact same follow-up.
    ///
    /// The core can halt in either ARM or Thumb state depending on where it happened to be
    /// executing - convert to ARM state before any chain-1 sequence in this file (which all
    /// inject ARM-encoded instructions) is attempted. `status` is the Debug Status Register
    /// value observed at the moment the halt was confirmed (`DBGACK` just became set), so its
    /// `TBIT` reflects the real halt-time core state.
    ///
    /// BUG FOUND (2026-09-08): this conversion previously only ran from inside [`Self::halt`]'s
    /// own DBGACK poll loop - [`Self::latch_watchpoint_halt`] (the *only* path a hardware
    /// breakpoint/watchpoint-triggered halt goes through, via `Arm7tdmi::status()` in `mod.rs`)
    /// never called it at all. Since essentially every register-read investigation in this
    /// project's history (see `arm7_pending_resume_pc_lost_across_core_calls` memory) halted via
    /// a breakpoint at real, Thumb-mode application code (`arm_main`, `main`, ...) rather than
    /// `Core::halt()` on an already-running target, every single subsequent chain-1 register/
    /// memory access after such a halt was silently feeding 32-bit ARM-encoded instructions to a
    /// core still decoding 16-bit Thumb - explaining the long-unexplained "SP/CPSR garbage,
    /// looks like code-address-shaped values" symptom (the misdecoded ARM words drive real,
    /// Thumb-decoded side effects instead of the intended MRS/STMIA capture sequence). Fixed by
    /// extracting this method and calling it from both halt paths.
    ///
    /// `dbgrq` distinguishes *why* the core is halted - `true` from [`Self::halt`] (a DBGRQ-
    /// initiated halt, which can interrupt the pipeline at any point, not synchronized to an
    /// instruction boundary), `false` from [`Self::latch_watchpoint_halt`] (a breakpoint/
    /// watchpoint match, which always stops at a well-defined fetch boundary). This matters for
    /// the PC captured in the non-Thumb branch below - see its own comment.
    fn enter_debug_state(&mut self, status: u32, dbgrq: bool) -> Result<(), Arm7tdmiError> {
        if status & debug_status::TBIT != 0 {
            tracing::debug!("Halted in Thumb state, converting to ARM");
            let (r0, pc) = self.change_to_arm()?;
            // BUG FOUND (2026-09-08): a second real root cause of the long-standing
            // "download --start's teardown-halt lands near an interworking bx, corrupting the
            // resume" bug (see arm7_pending_resume_pc_lost_across_core_calls memory) - a DBGRQ
            // halt needs an *additional* PC correction beyond change_to_arm's own `-0xa`,
            // confirmed against OpenOCD's `arm7_9_debug_entry`: `context[15] -=
            // arm7_9->dbgreq_adjust_pc * 2` for a Thumb-state DBGRQ halt specifically (not for a
            // breakpoint/watchpoint one), where ARM7TDMI's own `dbgreq_adjust_pc = 2` (see
            // `arm7tdmi.c`) - i.e. `-4` for Thumb state, applied only when `dbgrq` is true. Not
            // yet independently hardware-verified for the Thumb case the way the ARM case below
            // was (this file's own DBGRQ-halt bug reproduction happened to land in ARM state) -
            // kept for consistency with OpenOCD's reference and the same underlying mechanism,
            // flagged for future verification.
            let pc = if dbgrq { pc.wrapping_sub(4) } else { pc };
            // `BX R0` (with R0=0, per `change_to_arm`'s implementation) leaves the
            // CPU's PC at address 0 (low ROM) - restore the ORIGINAL r0/pc, exactly as
            // OpenOCD's `arm7_9_debug_entry` does via its register cache, before any
            // further chain-1 operation is attempted. `write_core_register(15, ..)`
            // itself resets `pending_resume_thumb` to `false` (see its own doc comment -
            // it must, for its *other*, more common caller: an explicit redirect to a
            // fresh, unrelated, ARM-mode target) - so the flag below is set *after*
            // this, not before, or this restore call would immediately clobber it back.
            self.write_core_register_unchecked(0, r0)?;
            self.write_core_register_unchecked(15, pc)?;
            // Remember to resume via `branch_resume_thumb_aware`'s real `BX`-based
            // interworking - see `Arm7tdmiDebugInterfaceState::pending_resume_thumb`'s
            // doc comment for why no CPSR capture is needed here at all.
            self.state.pending_resume_thumb = true;
        } else {
            self.state.pending_resume_thumb = false;
            // Cache the real, freshly-halted PC for a later bare `resume()` to reuse -
            // symmetric with the Thumb branch above (which caches its own via
            // `write_core_register(15, pc)`). Without this, a `resume()` with nothing
            // else writing PC in between falls back to re-reading "current PC" *after*
            // whatever chain-1 operations happened while halted (e.g. a plain memory
            // `read`/`write` command's own debug-speed instruction injection) have
            // already disturbed the pipeline - resuming from that post-operation PC,
            // rather than the real pre-halt one, would not land back in the target's
            // real code. See `Arm7tdmiDebugInterfaceState::pending_resume_pc`'s doc
            // comment.
            //
            // BUG FOUND (2026-09-08), root-caused precisely: `read_core_register(15)`'s own
            // `-12` correction (matching OpenOCD's generic `context[15] -= 3 * 4` STM-capture-lag
            // fix) is not the whole story for a DBGRQ-initiated halt specifically. OpenOCD's
            // `arm7_9_debug_entry` applies a *second*, DBGRQ-only correction on top:
            // `context[15] -= arm7_9->dbgreq_adjust_pc * 4` for ARM state, with ARM7TDMI's own
            // `dbgreq_adjust_pc = 2` (`arm7tdmi.c`) - i.e. an extra `-8`. Confirmed via a real,
            // reproducible hardware failure: `download --start`'s teardown (`Session::drop` ->
            // `halted_access` -> this exact DBGRQ `halt()` path) repeatedly cached
            // `pending_resume_pc = 0x40086c` when the core was actually halted mid-fetch of
            // `bx ip` at `0x400864` (`_begin`'s ARM->Thumb interworking jump into `main`) -
            // `0x40086c` is exactly `0x400864 + 8`, and is itself the literal-pool data word at
            // the `_undefined_instruction` label (not real code), so resuming there executed
            // garbage. `0x40086c - 8 = 0x400864` matches the real instruction address exactly.
            //
            // BUG FOUND (2026-09-08), root-caused precisely, watchpoint side: a watchpoint
            // (non-DBGRQ) ARM-state halt needs its own extra, *different* correction, confirmed
            // via a direct wire-trace/ground-truth comparison against OpenOCD (see
            // `cached_halt_pc`'s doc comment): a fetch watchpoint armed on the SVC vector
            // (`0x00000008`) and genuinely, cleanly hit (DebugStatus showed DBGACK+SYSCOMP in
            // both tools) had this file's `read_core_register(15)?` - already carrying its own
            // generic `-12` STM-capture-lag correction - report `0x00000014`, a further, fixed
            // `+12` past the real match address, not the `+8` a live/DBGRQ "architectural PC"
            // read would suggest. Reproducible and deterministic (not the continuously-drifting
            // artifact a *second* read on top of this one would show - see `cached_halt_pc`),
            // so this is a real, fixed pipeline-depth difference for how EmbeddedICE reports PC
            // specifically coming out of a watchpoint match, not noise.
            //
            // BUG FOUND (2026-09-10): this must be the *unchecked* read, exactly as
            // `read_core_register_unchecked`'s own doc comment already documents for this call
            // site - going through the checked `read_core_register` recurses back into
            // `ensure_halted` -> (if DBGACK reads flaky/cleared) `halt` -> this same
            // `enter_debug_state` branch -> checked `read_core_register` again. Confirmed on real
            // hardware: a `read` hit exactly this cycle (`ensure_halted`/`halt`/`read_core_register`
            // alternating 3423 times in the captured stack) and blew the stack instead of
            // returning `CoreNotHalted` once `ensure_halted`'s own one-retry budget was exhausted.
            //
            // BUG FOUND (2026-09-10), same day follow-up: going strictly unchecked here also
            // silently dropped a real, load-bearing side effect the old checked path had via
            // `ensure_halted`'s one-time retry - resilience against DBGACK intermittently
            // reading back cleared for a brief window right after a halt has just been
            // confirmed, before settling durably (the same signature already documented on
            // `ensure_halted` itself, and reproduced independently today via `erase`/
            // `debug --launch`). Confirmed as a real regression via a plain `download`: this
            // exact site captured a garbage PC (`0xf0e897ec` - not a plausible address on this
            // chip at all) during the flash algorithm's own load-time attach halt, which was
            // then used as the teardown resume target and caused the whole flash operation to
            // fail with a spurious Init() timeout. Restore the resilience without reintroducing
            // the unbounded-recursion hazard above: re-latch and retry the capture itself
            // (bounded, and never calling back into `halt()`/`ensure_halted`) if `is_halted()` -
            // a leaf status read with no capture side effects of its own - shows DBGACK has
            // already dropped by the time the capture is attempted.
            let mut pc = self.read_core_register_unchecked(15)?;
            for attempt in 0..3 {
                if self.is_halted()? {
                    break;
                }
                tracing::debug!(
                    "DBGACK dropped after halt during PC capture (attempt {attempt}), \
                     re-latching and retrying"
                );
                self.write_debug_control(debug_control::STICKY_HALT | debug_control::INTDIS)?;
                pc = self.read_core_register_unchecked(15)?;
            }
            let pc = if dbgrq {
                pc.wrapping_sub(8)
            } else {
                pc.wrapping_sub(12)
            };
            self.cache_resume_pc(pc);
        }
        Ok(())
    }

    /// Resume the core by clearing DBGRQ.
    ///
    /// Before doing that, redirects the fetch/decode pipeline to the intended PC via a real
    /// branch instruction ([`Self::write_pc`] immediately followed by [`Self::branch_resume`],
    /// with nothing else in between so the branch's calibrated offset still lands correctly).
    /// Confirmed necessary on real hardware - without it, `RESTART` let the core run for only
    /// a handful of instructions (whatever was left over in the pipeline from the debug-speed
    /// register-write sequence) before it re-entered debug state on its own, nowhere near
    /// either the call's entry point or its intended completion breakpoint; the result
    /// register then read back as whatever value had been written into it as an argument,
    /// unmodified - not the routine's real return value. Matches OpenOCD's `arm7_9_resume`,
    /// which always ends `restore_context` by re-writing PC (`dirty` or not) and immediately
    /// calls `branch_resume`, before ever touching the debug control register or issuing
    /// `RESTART`.
    pub fn resume(&mut self) -> Result<(), Arm7tdmiError> {
        tracing::info!("Resuming ARM7TDMI core");

        // Use whichever PC was most recently, explicitly written (the common case: the caller
        // just set up a routine call). Fall back to reading the current architectural PC for a
        // plain "continue from wherever it's halted" resume, where nothing wrote R15 first.
        let pc = match self.state.pending_resume_pc.take() {
            Some(pc) => {
                tracing::debug!("resume() using cached pending_resume_pc={:#x}", pc);
                pc
            }
            None => {
                let pc = self.read_core_register(15)?;
                tracing::debug!("resume() no cached pc, re-read current pc={:#x}", pc);
                pc
            }
        };
        // If the core was halted in Thumb state, a plain `write_pc`+`branch_resume` (a bare ARM
        // `B`, no interworking) would leave the CPU in ARM state trying to decode the target's
        // real Thumb-encoded code as ARM opcodes - see `branch_resume_thumb_aware`.
        let pending_thumb = std::mem::take(&mut self.state.pending_resume_thumb);
        match pending_thumb {
            true => {
                tracing::debug!("resume() restoring Thumb state via BX, pc={:#x}", pc);
                self.branch_resume_thumb_aware(pc)?;
            }
            false => {
                self.write_pc(pc)?;
                self.branch_resume()?;
            }
        }

        // Clear DBGRQ bit in Debug Control Register
        self.write_debug_control(0)?;

        // Use RESTART instruction - see `select_ir_idle`'s own doc comment.
        self.select_ir_idle(JtagInstruction::Restart)?;

        self.state.in_debug_state = false;
        tracing::debug!("Core resumed");
        Ok(())
    }

    /// Cache `pc` as [`Arm7tdmiDebugInterfaceState::pending_resume_pc`], so a later bare
    /// `resume()` (no explicit PC write in between) reuses this exact value instead of
    /// performing its own separate chain-1 PC capture.
    ///
    /// Used by [`crate::architecture::arm7::Arm7tdmi::halt`]/`reset_and_halt`, which both read
    /// PC right after halting purely to build their own `CoreInformation` return value - a read
    /// whose result many callers (e.g. `Session::halted_access`) never use. Per this file's own
    /// established finding that an extra register read can itself nudge the pipeline forward on
    /// this silicon (see `probe_rs_arm7_chain1_bug` memory, "core.step() genuinely works"
    /// section), letting `resume()` perform a *second*, independent PC read afterward compounds
    /// that same effect for no reason, when the value it would read is already known from the
    /// halt that just happened.
    pub(crate) fn cache_resume_pc(&mut self, pc: u32) {
        self.state.pending_resume_pc = Some(pc);
    }

    /// Peek at the cached halt-time PC (see [`Self::cache_resume_pc`]) without consuming it and
    /// without issuing a fresh chain-1 read.
    ///
    /// ROOT CAUSE (2026-09-08) of the SVC-vector-catch mechanism's failure to recognise a
    /// watchpoint hit deep in ROM: `status()`'s own `pc == SVC_VECTOR_ADDRESS` check used to do
    /// its own independent `read_core_reg(PC.id)` call - a *second* chain-1 register read after
    /// the one `enter_debug_state` already performs (via `read_core_register(15)` +
    /// `cache_resume_pc`) while latching the halt. Per this file's own established finding that
    /// every chain-1 register read genuinely, physically advances this silicon's pipeline by a
    /// few instructions (see the `read_core_registers`/`step` doc comments and the
    /// `probe_rs_arm7_chain1_bug`/`step_wreg_pc_anomaly_explained` memory entries), that second
    /// read never saw the true halt-time PC at all - it saw whatever the *first* read had already
    /// nudged it to, plus its own further nudge. Confirmed via a direct wire-trace diff against a
    /// from-scratch OpenOCD ground-truth test that halts cleanly and reports `pc: 0x00000008`
    /// exactly: DebugStatus showed a clean DBGACK+SYSCOMP halt in both cases, but probe-rs's own
    /// *second* PC read reported `0x20` - not a real position, just this read's own contribution
    /// on top of whatever the first read left behind. Consumers that need "where did the core
    /// just halt" should use this cached value (set once, by the same single read that already
    /// has to happen to prime a bare `resume()`) instead of reading PC again.
    pub(crate) fn cached_halt_pc(&self) -> Option<u32> {
        self.state.pending_resume_pc
    }

    /// Check if core is halted by reading Debug Status Register.
    ///
    /// If halted, also latches the halt via the Debug Control Register (`STICKY_HALT`/`INTDIS`
    /// set, `DBGRQ` cleared) - matching OpenOCD's `arm7_9_debug_entry`, whose *very first* step,
    /// unconditionally, on every debug entry regardless of what caused it (watchpoint match,
    /// DBGRQ, vector catch), is exactly this write, before anything else is done.
    ///
    /// Confirmed on real hardware to matter, not just be redundant bookkeeping: without it, a
    /// watchpoint-induced halt (e.g. the flash algorithm's completion breakpoint catching a
    /// real `BX LR` return) can report `DBGACK` set here - genuinely, if briefly - while the
    /// core is not durably captured for chain-1 (debug-speed) operations, and resumes running
    /// for real shortly after on its own. A caller that trusts this function's `true` result and
    /// proceeds straight to chain-1 reads/writes (as `wait_for_completion`'s poll loop, and
    /// `call_function`'s own register setup, both do) can end up injecting into a pipeline the
    /// core silently isn't actually listening to - explaining a class of previously-unexplained
    /// "successful" completions where the read-back register state didn't correspond to
    /// anything the routine plausibly left behind (see `probe_rs_arm7_chain1_bug` memory,
    /// "narrows the remaining probe-rs mystery" era, and the `program_page` false-completion
    /// investigation before this fix).
    pub fn is_halted(&mut self) -> Result<bool, Arm7tdmiError> {
        let status = self.read_debug_status()?;
        let halted = status & debug_status::DBGACK != 0;
        self.state.in_debug_state = halted;
        Ok(halted)
    }

    /// Precondition check used by every chain-1/system-speed operation that requires the core to
    /// already be halted (register/memory access) - errors with [`Arm7tdmiError::CoreNotHalted`]
    /// if it genuinely isn't, after trying to recover once.
    ///
    /// A halt reported successful by [`Self::halt`] (including right after attach, see
    /// [`super::Arm7tdmi::new`]) has been observed, on real hardware, to sometimes not durably
    /// stick for more than a short window - specifically following this project's flash
    /// algorithm having just run (e.g. a plain `read`/`write` immediately after `erase`/
    /// `download`): DBGACK can read cleared again a short time later with no explanation other
    /// than genuine hardware/timing flakiness in this exact scenario, causing whatever the very
    /// next real operation happens to be to fail this precondition, even though the core was
    /// durably confirmed halted just moments before. Re-halting once before giving up recovers
    /// this specific case in practice; a caller that hits this after a *bare* attach with no
    /// halt at all yet either way ends up with the correct, honest `CoreNotHalted` error once
    /// this retry is also exhausted.
    fn ensure_halted(&mut self) -> Result<(), Arm7tdmiError> {
        if self.is_halted()? {
            return Ok(());
        }
        tracing::debug!("Core unexpectedly not halted, retrying halt once before failing");
        self.halt()?;
        if self.is_halted()? {
            return Ok(());
        }
        Err(Arm7tdmiError::CoreNotHalted)
    }

    /// Genuinely, durably latch a halt that was detected via a watchpoint match (as opposed to
    /// [`Self::halt`]'s own DBGRQ-initiated halt), by momentarily asserting `DBGRQ` and then
    /// switching to `STICKY_HALT` - mirroring [`Self::halt`]'s own proven-reliable sequence
    /// exactly, just entered from a different starting condition.
    ///
    /// Confirmed on real hardware to matter: after a watchpoint match, `is_halted()` reporting
    /// `DBGACK` set does not, on its own, durably stop the core on this silicon - it can keep
    /// running for real afterward (confirmed by re-reading PC after a plain, JTAG-idle sleep
    /// with no further access at all, and seeing it advance). Just writing
    /// `STICKY_HALT | INTDIS` directly, without ever having asserted `DBGRQ` first, does not fix
    /// this - only going through the same DBGRQ-then-STICKY_HALT transition `halt()` itself uses
    /// does. Call this once, right when a poll loop first observes the halt it was waiting for -
    /// not from inside [`Self::is_halted`] itself, which is also used as a precondition check
    /// throughout chain-1 operations where momentarily asserting DBGRQ has been confirmed to
    /// cause real regressions (e.g. bulk memory writes silently not landing).
    pub(crate) fn latch_watchpoint_halt(&mut self) -> Result<(), Arm7tdmiError> {
        // Read status *before* asserting DBGRQ/STICKY_HALT below: TBIT reflects the real
        // architectural state the core was executing in at the watchpoint match, and this
        // function's own writes don't change it, but reading it fresh here (rather than reusing
        // a possibly-stale caller-side value) keeps this self-contained and correct regardless
        // of caller. See `enter_debug_state`'s doc comment for why this call is required here at
        // all - this was the actual root cause of the long-unresolved SP/CPSR bug.
        let status = self.read_debug_status()?;
        self.write_debug_control(debug_control::DBGRQ)?;
        self.write_debug_control(debug_control::STICKY_HALT | debug_control::INTDIS)?;
        // `dbgrq: false` - the momentary DBGRQ assertion just above is only this function's own
        // *latching* mechanism for a halt the watchpoint match already caused; the core's real
        // PC was already frozen by that match, at a well-defined fetch boundary, before this
        // function ever ran - matching OpenOCD's own `debug_reason`-based (not
        // DBGRQ-bit-based) distinction. See `enter_debug_state`'s doc comment.
        self.enter_debug_state(status, false)?;
        Ok(())
    }

    /// Like [`Self::is_halted`], but also requires `SYSCOMP` (matching `system_speed_access`'s
    /// own completion check and OpenOCD's `arm7_9_execute_sys_speed`, which `arm7_9_step` uses
    /// to wait for a step to complete) rather than a generic DBGACK-only halted check - DBGACK
    /// alone can assert before the core has genuinely run to a `step()`-armed watchpoint match.
    pub(crate) fn is_halted_with_syscomp(&mut self) -> Result<bool, Arm7tdmiError> {
        let status = self.read_debug_status()?;
        let halted = status & (debug_status::DBGACK | debug_status::SYSCOMP)
            == (debug_status::DBGACK | debug_status::SYSCOMP);
        self.state.in_debug_state = halted;
        Ok(halted)
    }

    /// Perform a physical reset of the target via the probe's reset line (nSRST/nRST),
    /// then re-establish the JTAG TAP state.
    pub fn reset(&mut self) -> Result<(), Arm7tdmiError> {
        tracing::info!("Resetting ARM7TDMI target via probe reset line");
        // `JtagChain` only exposes the split assert/deassert calls (no combined `target_reset`),
        // so reproduce the same timing this board's own combined reset used: a 10ms assert pulse,
        // then 200ms for the target to actually come out of reset (re-run its boot ROM, etc.)
        // before any further JTAG activity is attempted - OpenOCD's known-good config for this
        // exact adapter/board combination (the `axm0432_jtag` FTDI layout) uses a 200ms
        // `jtag_ntrst_delay` for the same purpose.
        self.probe.target_reset_assert()?;
        std::thread::sleep(Duration::from_millis(10));
        self.probe.target_reset_deassert()?;
        std::thread::sleep(Duration::from_millis(200));

        // The reset invalidates any assumptions about the current TAP state.
        self.state.in_debug_state = false;

        self.init()
    }

    /// The number of hardware breakpoint/watchpoint units the EmbeddedICE module provides.
    pub const HW_BREAKPOINT_UNIT_COUNT: usize = 2;

    /// Configure hardware breakpoint/watchpoint unit `index` to halt the core when it fetches
    /// `address` (an instruction-fetch breakpoint, address bit 0 masked - see below).
    ///
    /// Register values/masks are taken from ARM DDI 0029G Appendix B / OpenOCD's
    /// `embeddedice.h` and the reset-vector-catch watchpoint example in `arm7_9_common.c`: the
    /// data bus is masked out entirely (data mask = all ones, i.e. don't care), enabled, and
    /// only the `nOPC` (opcode fetch) bit is compared so it triggers on instruction fetch rather
    /// than data access.
    ///
    /// The address mask is `1` (bit 0 don't-care), not an exact match (`0`) - confirmed via a
    /// real OpenOCD wire-trace ground truth (`jtag_decode.py`) that this is exactly what OpenOCD
    /// writes here (`arm7_9_set_breakpoint`), and load-bearing, not just defensive tolerance for
    /// a misaligned caller-supplied address: this crate's own long-standing "hardware breakpoints
    /// can't reliably catch instructions in a short window immediately after an ARM-to-Thumb
    /// interworking `bx`" limitation (see `arm7_pending_resume_pc_lost_across_core_calls` memory,
    /// in the `mc1322x-rs` project) turned out to be exactly this - `set_hw_breakpoint` with an
    /// exact-match mask never catches a real, even-aligned Thumb target address reached via an
    /// interworking transition, while OpenOCD's identical target/address, differing only in this
    /// one register value, reliably does. A mask of `1` is always safe regardless of instruction
    /// set: a real ARM fetch address is always a multiple of 4 (bit 0 hardwired to 0) and a real
    /// Thumb one a multiple of 2 (also bit 0 = 0), so this never causes two *distinct* real
    /// instructions to alias onto the same match - it only widens the comparator's tolerance for
    /// whatever bit-0 value the bus genuinely presents during the fetch this crate is trying to
    /// catch.
    pub fn set_hw_breakpoint(&mut self, index: usize, address: u32) -> Result<(), Arm7tdmiError> {
        self.configure_fetch_watchpoint(index, address, 1)?;
        tracing::debug!(
            "Set ARM7TDMI hardware breakpoint #{index} at address {:#010x}",
            address
        );
        Ok(())
    }

    /// Configure hardware breakpoint/watchpoint unit `index` to halt the core on the very next
    /// instruction fetch, regardless of address (address mask = all-ones, i.e. don't care).
    ///
    /// This is the standard EmbeddedICE technique for single-stepping without needing to
    /// decode the current instruction to predict the next PC (which would otherwise be wrong
    /// for any taken branch): resuming with this watchpoint armed halts the core again after
    /// exactly one instruction fetch.
    pub fn set_wildcard_breakpoint(&mut self, index: usize) -> Result<(), Arm7tdmiError> {
        self.configure_fetch_watchpoint(index, 0, 0xFFFF_FFFF)
    }

    fn configure_fetch_watchpoint(
        &mut self,
        index: usize,
        address: u32,
        address_mask: u32,
    ) -> Result<(), Arm7tdmiError> {
        let Some((addr_value, addr_mask, data_mask, control_value)) =
            EmbeddedIceRegister::watchpoint_unit(index)
        else {
            return Err(Arm7tdmiError::InvalidBreakpointUnit(index));
        };
        let control_mask = EmbeddedIceRegister::control_mask_register(index).unwrap();

        self.write_ice_register(addr_value, address)?;
        self.write_ice_register(addr_mask, address_mask)?;
        self.write_ice_register(data_mask, 0xFFFF_FFFF)?;
        self.write_ice_register(control_value, watchpoint_control::ENABLE)?;
        self.write_ice_register(control_mask, !watchpoint_control::N_OPC & 0xFF)?;
        Ok(())
    }

    /// Configures both hardware watchpoint units together as a "range-chained" single-step
    /// trigger, matching OpenOCD's `arm7_9_enable_eice_step` exactly.
    ///
    /// A lone watchpoint (wildcard *or* exact-match) armed only one instruction ahead of where
    /// `resume()` is about to redirect execution does not reliably fire on real ARM7TDMI
    /// silicon - confirmed directly: `step()` previously used a wildcard match on a single
    /// unit, and was observed advancing PC by a large, constant, multi-instruction amount per
    /// call instead of stopping after exactly one instruction (see `step_sim`'s module doc and
    /// `probe_rs_arm7_chain1_bug` memory) - the watchpoint comparator apparently needs the core
    /// to have genuinely resumed (past the RESTART/pipeline-flush transient) before it can
    /// validly evaluate a match, which a target only one instruction away races.
    ///
    /// The fix (from ARM DDI 0029G's watchpoint chaining feature, used by every working ARM7/9
    /// JTAG debugger for exactly this purpose): comparator 1 (`Watchpoint1`) is set to an
    /// *exact* match on the *current* PC (not `next_pc`) - its own `ENABLE` bit is left clear,
    /// so it never directly triggers a halt, but its "range" output is still computed and feeds
    /// comparator 0's "rangein" (via comparator 0's `control_mask` excluding the `RANGE` bit,
    /// making comparator 0's own match conditional on it). Comparator 0 (`Watchpoint0`) is a
    /// wildcard (any address) instruction-fetch match, `ENABLE`d - but gated behind comparator
    /// 1's range signal, it can only actually assert a breakpoint once comparator 1 has already
    /// matched the current instruction's own fetch once. This reliably catches "the very next
    /// fetch after this one", regardless of how few instructions away it is.
    ///
    /// If `next_pc == current_pc` (the current instruction branches to itself, e.g. an
    /// infinite-loop trap), the chained configuration above can never see the required
    /// "comparator 1 matches, *then* something else fetches" transition (there's no *later*
    /// fetch to gate) - OpenOCD's `else` branch handles this by disabling comparator 0 entirely
    /// and using comparator 1 as an ordinary, directly-`ENABLE`d exact-match breakpoint instead
    /// (fine here: matching the same address repeatedly is exactly what's wanted).
    ///
    /// Also records `current_pc` as [`Arm7tdmiDebugInterfaceState::pending_resume_pc`], so the
    /// `resume()` that the caller is about to issue takes its explicit-PC-write path rather
    /// than its "no pending PC, re-read the architectural PC" fallback (which would otherwise
    /// re-select scan chain 1 for a register capture *after* these scan-chain-2 watchpoint
    /// writes, before `resume()`'s own chain-2 `write_debug_control`/RESTART sequence - this
    /// file's very first resolved bug was exactly this kind of unconditional scan-chain
    /// reselection breaking EmbeddedICE state; avoiding it here rather than relying on the
    /// reselection-caching fix for that bug to also cover this specific ordering).
    pub(crate) fn configure_step_watchpoints(
        &mut self,
        current_pc: u32,
        next_pc: u32,
    ) -> Result<(), Arm7tdmiError> {
        self.state.pending_resume_pc = Some(current_pc);
        if next_pc != current_pc {
            self.write_ice_register(EmbeddedIceRegister::Watchpoint0AddressMask, 0xFFFF_FFFF)?;
            self.write_ice_register(EmbeddedIceRegister::Watchpoint0DataMask, 0xFFFF_FFFF)?;
            self.write_ice_register(
                EmbeddedIceRegister::Watchpoint0ControlValue,
                watchpoint_control::ENABLE,
            )?;
            self.write_ice_register(
                EmbeddedIceRegister::Watchpoint0ControlMask,
                !(watchpoint_control::RANGE | watchpoint_control::N_OPC) & 0xFF,
            )?;

            self.write_ice_register(EmbeddedIceRegister::Watchpoint1AddressValue, current_pc)?;
            self.write_ice_register(EmbeddedIceRegister::Watchpoint1AddressMask, 0)?;
            self.write_ice_register(EmbeddedIceRegister::Watchpoint1DataMask, 0xFFFF_FFFF)?;
            self.write_ice_register(EmbeddedIceRegister::Watchpoint1ControlValue, 0)?;
            self.write_ice_register(
                EmbeddedIceRegister::Watchpoint1ControlMask,
                !watchpoint_control::N_OPC & 0xFF,
            )?;
        } else {
            self.write_ice_register(EmbeddedIceRegister::Watchpoint0AddressMask, 0xFFFF_FFFF)?;
            self.write_ice_register(EmbeddedIceRegister::Watchpoint0DataMask, 0xFFFF_FFFF)?;
            self.write_ice_register(EmbeddedIceRegister::Watchpoint0ControlValue, 0)?;
            self.write_ice_register(EmbeddedIceRegister::Watchpoint0ControlMask, 0xFF)?;

            self.write_ice_register(EmbeddedIceRegister::Watchpoint1AddressValue, next_pc)?;
            self.write_ice_register(EmbeddedIceRegister::Watchpoint1AddressMask, 0)?;
            self.write_ice_register(EmbeddedIceRegister::Watchpoint1DataMask, 0xFFFF_FFFF)?;
            self.write_ice_register(
                EmbeddedIceRegister::Watchpoint1ControlValue,
                watchpoint_control::ENABLE,
            )?;
            self.write_ice_register(
                EmbeddedIceRegister::Watchpoint1ControlMask,
                !watchpoint_control::N_OPC & 0xFF,
            )?;
        }
        Ok(())
    }

    /// Disable hardware breakpoint/watchpoint unit `index`.
    pub fn clear_hw_breakpoint(&mut self, index: usize) -> Result<(), Arm7tdmiError> {
        let Some((_, _, _, control_value)) = EmbeddedIceRegister::watchpoint_unit(index) else {
            return Err(Arm7tdmiError::InvalidBreakpointUnit(index));
        };

        self.write_ice_register(control_value, 0)?;
        tracing::debug!("Cleared ARM7TDMI hardware breakpoint #{index}");
        Ok(())
    }

    /// Get the current state
    pub fn state(&self) -> &Arm7tdmiDebugInterfaceState {
        &*self.state
    }
}

impl fmt::Debug for Arm7tdmiCommunicationInterface<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Arm7tdmiCommunicationInterface")
            .field("state", &self.state)
            .finish()
    }
}
