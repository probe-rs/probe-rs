//! ARM7TDMI JTAG communication interface via EmbeddedICE
//!
//! Implements the EmbeddedICE debug protocol over scan chain 1 (data/instruction bus, used to
//! single-step the 3-stage pipeline and observe/inject bus values) and scan chain 2 (the
//! EmbeddedICE register file: debug control/status and the two watchpoint units).
//!
//! Based on ARM DDI 0029G ("ARM7TDMI (Rev 3) TRM"), Appendix B "Debug in Depth", and OpenOCD's
//! `arm7tdmi.c`, `arm7_9_common.c` and `embeddedice.c`, cited by function name below.

use crate::{
    Error,
    architecture::arm::ArmError,
    probe::{BitSequence, DebugProbeError, JtagBatch, JtagChain, TapState},
};

use bitvec::field::BitField;
use std::fmt;
use std::time::{Duration, Instant};

/// Length of the ARM7TDMI instruction register.
const IR_LEN: usize = 4;

/// CPSR's T (Thumb state) bit.
pub(super) const CPSR_TBIT: u32 = 1 << 5;

/// [`HALT_RECORD_TAG`] value marking a valid halt record (bit 0: Thumb state), see
/// [`Arm7tdmiCommunicationInterface::record_halt_state`].
const HALT_RECORD_MAGIC: u32 = 0xA7D1_6000;
/// Registers holding the halt record: the data-value registers of both units. No unit
/// configuration uses them (the data bus is always masked out) and only a power cycle clears
/// them, so they outlive the probe-rs process that wrote them.
const HALT_RECORD_PC: EmbeddedIceRegister = EmbeddedIceRegister::Watchpoint0DataValue;
const HALT_RECORD_TAG: EmbeddedIceRegister = EmbeddedIceRegister::Watchpoint1DataValue;
/// Index of the data value register in [`EmbeddedIceRegister::unit_registers`].
const UNIT_DATA_VALUE: usize = 2;

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

    /// All six registers of hardware unit `index` (0 or 1): address value/mask, data
    /// value/mask, control value/mask.
    fn unit_registers(index: usize) -> Option<[Self; 6]> {
        match index {
            0 => Some([
                Self::Watchpoint0AddressValue,
                Self::Watchpoint0AddressMask,
                Self::Watchpoint0DataValue,
                Self::Watchpoint0DataMask,
                Self::Watchpoint0ControlValue,
                Self::Watchpoint0ControlMask,
            ]),
            1 => Some([
                Self::Watchpoint1AddressValue,
                Self::Watchpoint1AddressMask,
                Self::Watchpoint1DataValue,
                Self::Watchpoint1DataMask,
                Self::Watchpoint1ControlValue,
                Self::Watchpoint1ControlMask,
            ]),
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
/// Bit assignments match OpenOCD's `embeddedice.h` (`EICE_DBG_STATUS_*`).
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
    /// Sticky halt latch (OpenOCD's `EICE_DBG_CONTROL_DBGACK`, not the status register's bit 0).
    ///
    /// Set after halting so the core stays in debug state without DBGRQ asserted. While set,
    /// `RESTART` has no effect and `SYSCOMP` never sets, so it must be cleared around
    /// system-speed accesses (see `system_speed_access`).
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
/// While halted, each chain-1 shift followed by Update-DR clocks the core once. A single-word
/// load/store takes 4 clocks: fetch, decode, execute, then the data cycle during which the
/// debugger captures or injects the bus value. See ARM DDI 0029G Appendix B and OpenOCD's
/// `arm7tdmi_clock_out`/`arm7tdmi_clock_data_in`.
mod arm7_instructions {
    /// MOV r0, r0 (NOP)
    pub const NOP: u32 = 0xE1A0_0000;
    /// `LDMIA R0, {<reglist>}` template (no write-back); OR in `1 << reg`. Injects a literal
    /// into a register without a real memory access, like OpenOCD's `arm7tdmi_write_core_regs`.
    pub const LOAD_MULTIPLE_R0: u32 = 0xE890_0000;
    /// `STMIA R0, {<reglist>}` template (no write-back), like OpenOCD's
    /// `arm7tdmi_read_core_regs`. Used to capture register values.
    pub const STORE_MULTIPLE_R0: u32 = 0xE880_0000;
    /// `LDMIA R0!, {<reglist>}` template (write-back), like OpenOCD's `arm7tdmi_load_word_regs`.
    /// Used for real system-speed memory reads (see `read_memory_32`); OpenOCD never uses a
    /// single-word `LDR` for this.
    pub const LOAD_MULTIPLE_R0_WRITEBACK: u32 = 0xE8B0_0000;
    /// `STMIA R0!, {<reglist>}` template, like OpenOCD's `arm7tdmi_store_word_regs`. Used for
    /// real system-speed memory writes (see `write_memory_32`).
    pub const STORE_MULTIPLE_R0_WRITEBACK: u32 = 0xE8A0_0000;
    /// MRS R0, CPSR
    pub const MRS_R0_CPSR: u32 = 0xE10F_0000;
    /// MRS R0, SPSR (the banked SPSR of the current exception mode). Used by
    /// [`super::Arm7tdmiCommunicationInterface::return_from_exception`] to learn the state to
    /// resume into.
    pub const MRS_R0_SPSR: u32 = 0xE14F_0000;
    /// `STR R0, [R15]`, like OpenOCD's `arm7tdmi_read_xpsr`. Captures a value computed into R0
    /// (CPSR) where `STMIA R0, {R0}` would use that value as a (misaligned) base address; PC is
    /// always word-aligned.
    pub const STR_R0_R15: u32 = 0xE58F_0000;
    /// `MSR CPSR_<field>, #(imm8 ROR rotate_imm*2)` template.
    /// OR in `(field_mask << 16) | (rotate_imm << 8) | imm8`.
    pub const MSR_CPSR_IMM: u32 = 0xE320_F000;
    /// `BX R0`: sets PC and the T bit from R0 atomically. Used by
    /// [`super::Arm7tdmiCommunicationInterface::branch_resume_thumb_aware`] to resume into
    /// Thumb code, like OpenOCD's `arm7tdmi_branch_resume_thumb`; a plain `B` cannot interwork.
    pub const BX_R0: u32 = 0xE12F_FF10;
    /// `B PC-16` (offset -6 words), like OpenOCD's `arm7tdmi_branch_resume`
    /// (`ARMV4_5_B(0xfffffa, 0)`). Its offset is calibrated to land on the value
    /// [`super::Arm7tdmiCommunicationInterface::write_pc`] just injected, so it must follow
    /// `write_pc` with no other chain-1 operation in between. The branch is needed to flush
    /// stale prefetched instructions before `RESTART`; an injected PC alone is not enough.
    pub const BRANCH_BACK_TO_PC: u32 = 0xEAFF_FFFA;
}

/// Thumb encodings used by [`Arm7tdmiCommunicationInterface::change_to_arm`] and
/// `branch_resume_thumb_aware`.
///
/// A Thumb core fetches two halfwords per 32-bit bus cycle, so each opcode is duplicated into
/// both halves, like OpenOCD's `ARMV4_5_T_*` macros (`arm_opcodes.h`).
mod thumb_instructions {
    /// MOV r8, r8 (NOP), duplicated into both halfwords.
    pub const NOP: u32 = 0x46C0_46C0;
    /// `STR r0, [r0]`, duplicated into both halfwords.
    pub const STR_R0_R0: u32 = 0x6000_6000;
    /// MOV r0, r15 (r0 = pc), duplicated into both halfwords.
    pub const MOV_R0_R15: u32 = 0x4678_4678;
    /// BX r0, duplicated into both halfwords - matches OpenOCD's `ARMV4_5_T_BX(0)`.
    pub const BX_R0: u32 = 0x4700_4700;
    /// `LDR r0, [PC, #0]`, like OpenOCD's `ARMV4_5_T_LDR_PCREL(0)`. Restores r0 (clobbered as
    /// the `BX` target) by injecting the literal during the load's data cycle, the Thumb
    /// equivalent of [`super::Arm7tdmiCommunicationInterface::load_immediate`].
    pub const LDR_R0_PCREL: u32 = 0x4800_4800;
    /// `B PC-16` (offset `0x7f8` = -8 halfwords), like OpenOCD's
    /// `arm7tdmi_branch_resume_thumb`'s trailing `ARMV4_5_T_B(0x7f8)`. As with
    /// [`super::arm7_instructions::BRANCH_BACK_TO_PC`] the offset is calibrated, so it must be
    /// injected at the same clock distance as in OpenOCD.
    pub const BRANCH_BACK_TO_PC: u32 = 0xE7F8_E7F8;
}

/// The state information for the ARM7TDMI debug interface
#[derive(Debug, Default, Clone)]
pub struct Arm7tdmiDebugInterfaceState {
    /// Whether the core is known to be in debug state
    in_debug_state: bool,
    /// The value most recently written to R15 via
    /// [`Arm7tdmiCommunicationInterface::write_core_register`], if any. `resume()` redoes this
    /// PC write directly before [`Arm7tdmiCommunicationInterface::branch_resume`]. `None` means
    /// PC was not rewritten and `resume()` uses the current PC.
    pending_resume_pc: Option<u32>,
    /// Whether [`Arm7tdmiCommunicationInterface::halt`] found the core in Thumb state.
    ///
    /// Halt converts the core to ARM state via [`Arm7tdmiCommunicationInterface::change_to_arm`]
    /// but does not restore the T bit. If this is set, `resume()` must use
    /// `branch_resume_thumb_aware` (a real `BX`, like OpenOCD's `arm7tdmi_branch_resume_thumb`)
    /// because a plain ARM `B` cannot interwork. Nothing else touches CPSR in between, so only
    /// T needs restoring; OpenOCD likewise never writes T via MSR.
    pending_resume_thumb: bool,
    /// Whether debug-state entry ([`Arm7tdmiCommunicationInterface::enter_debug_state`] or
    /// `step()`'s completion path) already ran for the current halt. It must run once per halt:
    /// a second pass sees the core already in ARM state (losing `pending_resume_thumb`) and its
    /// fresh PC capture would overwrite the cached PC. Cleared by `resume()` and `reset()`.
    debug_entry_done: bool,
    /// The core's CPSR (T clear, the core is kept in ARM state while halted) for the current
    /// halt, used to detect data aborts from debugger memory accesses and to restore the mode.
    /// Cleared whenever the core runs or CPSR is written.
    halt_cpsr: Option<u32>,
    /// Whether the current halt was requested by this debugger (DBGRQ or the wildcard fetch
    /// watchpoint), as opposed to a breakpoint/watchpoint hit. Cleared when the core runs.
    halt_requested: bool,
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

    /// A debugger memory access raised a data abort
    #[error("Data abort while accessing memory at {address:#010x}")]
    DataAbort {
        /// The (first) address of the access.
        address: u32,
    },

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
    /// Borrowed from the session-lifetime state in
    /// [`crate::core::core_state::CombinedCoreState`]. This must not be a clone: the interface
    /// is rebuilt on every [`crate::Session::core`] call, and e.g. `pending_resume_pc` is written
    /// by one call (`Session::prepare_running_on_ram`) and consumed by a later `resume()`.
    state: &'probe mut Arm7tdmiDebugInterfaceState,
    /// The IR the TAP currently holds, if known (`None` forces a reselect). Like OpenOCD's
    /// `arm_jtag_set_instr` `cur_instr` check: consecutive chain-1 clocks must be plain DR
    /// shifts, reselecting IR each time makes chain 1 read back zero.
    current_instruction: Option<JtagInstruction>,
    /// The scan chain last selected via SCAN_N, if known (like OpenOCD's `arm_jtag_scann`
    /// `cur_scan_chain` cache).
    current_scan_chain: Option<ScanChain>,
}

impl<'probe> Arm7tdmiCommunicationInterface<'probe> {
    /// Create a new ARM7TDMI communication interface.
    ///
    /// Cheap and non-disruptive: this runs on every [`crate::Session::core`] call. The one-time
    /// TAP reset and unit clearing is done by `Self::init`, gated on the persisted flag in
    /// [`crate::architecture::arm7::Arm7tdmiState`] (see
    /// [`crate::architecture::arm7::Arm7tdmi::new`]).
    pub fn new(probe: JtagChain<'probe>, state: &'probe mut Arm7tdmiDebugInterfaceState) -> Self {
        Self {
            probe,
            state,
            current_instruction: None,
            current_scan_chain: None,
        }
    }

    /// Drop the cached IR and scan chain selection after a failed batch: the TAP may hold
    /// either the old or the new selection.
    fn forget_tap_selection(&mut self) {
        self.current_instruction = None;
        self.current_scan_chain = None;
    }

    /// Reset the JTAG TAP and read back its IDCODE.
    ///
    /// Unlike `Self::init` this does not touch the EmbeddedICE units, so it is safe for plain
    /// identification (`probe-rs info`).
    pub fn read_idcode(&mut self) -> Result<u32, Arm7tdmiError> {
        let mut batch = JtagBatch::new();
        self.probe.tap_reset(&mut batch);
        self.probe
            .run(batch)
            .inspect_err(|_| self.forget_tap_selection())?;
        // TAP reset invalidates the cached IR/scan chain selection.
        self.current_instruction = None;
        self.current_scan_chain = None;

        let idcode = self.scan_dr(JtagInstruction::Idcode, 0, 32)?;
        Ok(idcode as u32)
    }

    /// Initialize the debug interface: reset the TAP, verify the IDCODE, select scan chain 2 and
    /// clear both hardware units. Call once per session (see [`Self::new`]).
    pub(crate) fn init(&mut self) -> Result<(), Arm7tdmiError> {
        let idcode = self.read_idcode()?;
        tracing::debug!("ARM7TDMI IDCODE: 0x{:08X}", idcode);
        // IEEE 1149.1 requires IDCODE bit 0 set; all-ones or all-zeros means no TAP is answering
        // (wiring, power, or a JTAG clock too fast for the core).
        if idcode & 1 == 0 || idcode == u32::MAX {
            return Err(Arm7tdmiError::Other(format!(
                "No valid JTAG IDCODE ({idcode:#010x}): check the connection and power, or lower \
                 the JTAG clock (--speed)"
            )));
        }

        self.select_scan_chain(ScanChain::Chain2)?;

        // The watchpoint units survive a JTAG reconnect; a stale armed unit could re-halt the
        // core right after RESTART (DBGACK without SYSCOMP), so clear both.
        for index in 0..Self::HW_BREAKPOINT_UNIT_COUNT {
            if let Err(e) = self.clear_hw_breakpoint(index) {
                tracing::warn!("failed to clear watchpoint unit {index}: {e:?}");
            }
        }

        tracing::debug!("ARM7TDMI communication interface initialized");
        Ok(())
    }

    /// Select `instruction` and shift `dr_bits` bits of `data` through the DR, returning the
    /// previously captured content. Ends in Run-Test/Idle via Update-DR.
    ///
    /// The IR-select is skipped if `instruction` is already selected (see
    /// `current_instruction`).
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
        let mut results = self
            .probe
            .run(batch)
            .inspect_err(|_| self.forget_tap_selection())?;
        let captured = results
            .take(handle)
            .map_err(|_| Arm7tdmiError::Other("missing JTAG capture result".to_string()))?;

        Ok(captured.as_bits().load_le::<u64>())
    }

    /// Like [`Self::scan_dr`], but ends the shift in Pause-DR and then walks to Run-Test/Idle.
    /// Used for chain-1 clocking only (see [`Self::clock1`]).
    fn scan_dr_pause_idle(
        &mut self,
        instruction: JtagInstruction,
        data: u64,
        dr_bits: u32,
    ) -> Result<u64, Arm7tdmiError> {
        let dr_seq = BitSequence::from_u64(dr_bits as usize, data);

        let mut batch = JtagBatch::new();
        if self.current_instruction != Some(instruction) {
            // Park in Pause-DR after the IR select, like OpenOCD's
            // `arm_jtag_set_instr(..., TAP_DRPAUSE)`.
            let ir_seq = BitSequence::from_u64(IR_LEN, instruction as u64);
            self.probe.shift_ir(&mut batch, &ir_seq);
            batch.enter(TapState::PauseDr);
            self.current_instruction = Some(instruction);
        }
        let handle = self.probe.exchange_dr(&mut batch, &dr_seq);
        batch.enter(TapState::PauseDr);
        self.probe.run_test_idle(&mut batch, 0);
        let mut results = self
            .probe
            .run(batch)
            .inspect_err(|_| self.forget_tap_selection())?;
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
        self.probe
            .run(batch)
            .inspect_err(|_| self.forget_tap_selection())?;
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
        let mut results = self
            .probe
            .run(batch)
            .inspect_err(|_| self.forget_tap_selection())?;
        let captured = results
            .take(handle)
            .map_err(|_| Arm7tdmiError::Other("missing JTAG capture result".to_string()))?;

        Ok(captured.as_bits().load_le::<u64>())
    }

    /// Select a scan chain via the SCAN_N instruction.
    ///
    /// Skipped if `chain` is already selected, like OpenOCD's `arm_jtag_scann`. Selecting other
    /// instructions in between (e.g. RESTART) does not change the SCAN_N selection.
    fn select_scan_chain(&mut self, chain: ScanChain) -> Result<(), Arm7tdmiError> {
        if self.current_scan_chain == Some(chain) {
            return Ok(());
        }
        let chain_num = chain as u8;
        // Both scans park in Pause-DR, like OpenOCD's `arm_jtag_scann(..., TAP_DRPAUSE)`.
        let mut batch = JtagBatch::new();
        let ir_seq = BitSequence::from_u64(IR_LEN, JtagInstruction::ScanN as u64);
        self.probe.shift_ir(&mut batch, &ir_seq);
        batch.enter(TapState::PauseDr);
        let dr_seq = BitSequence::from_u64(4, chain_num as u64);
        let _ = self.probe.exchange_dr(&mut batch, &dr_seq);
        batch.enter(TapState::PauseDr);
        self.probe
            .run(batch)
            .inspect_err(|_| self.forget_tap_selection())?;
        self.current_instruction = Some(JtagInstruction::ScanN);
        self.current_scan_chain = Some(chain);
        tracing::trace!("Selected scan chain {}", chain_num);
        Ok(())
    }

    /// Read an EmbeddedICE register (scan chain 2).
    ///
    /// Capture-DR returns the content from before the current shift, so the first 38-bit scan
    /// (data, 5-bit address, R/W) only latches the address and a second scan captures the
    /// value, like OpenOCD's `embeddedice_read_reg_w_check`.
    fn read_ice_register(&mut self, register: EmbeddedIceRegister) -> Result<u32, Arm7tdmiError> {
        self.select_scan_chain(ScanChain::Chain2)?;

        let address = register as u64;
        // Bit 37 = R/W (0 = read), bits [36:32] = address, bits [31:0] = data.
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
        // The second scan commits the write.
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

    /// Shift one value through scan chain 1 (data bus plus BREAKPT), clocking the core once,
    /// and return the bus content captured from the previous clock.
    ///
    /// Shift order (ARM DDI 0029G B.1.1, reversed from the documented TDI-to-TDO layout):
    /// BREAKPT first, then data bits 31 down to 0.
    ///
    /// Each shift ends in Pause-DR and rests in Run-Test/Idle before the next, like OpenOCD's
    /// chain-1 clocking. Without the idle rest chain 1 reads back a frozen value.
    fn clock1(&mut self, breakpt: bool, data: u32) -> Result<u32, Arm7tdmiError> {
        self.select_scan_chain(ScanChain::Chain1)?;

        let value = (breakpt as u64) | ((data.reverse_bits() as u64) << 1);
        let captured = self.scan_dr_pause_idle(JtagInstruction::Intest, value, 33)?;
        let captured_data = ((captured >> 1) & 0xFFFF_FFFF) as u32;
        Ok(captured_data.reverse_bits())
    }

    /// Like [`Self::clock1`], but without capturing the data bus. Use for filler clocks.
    fn clock1_no_capture(&mut self, breakpt: bool, data: u32) -> Result<(), Arm7tdmiError> {
        self.select_scan_chain(ScanChain::Chain1)?;

        let value = (breakpt as u64) | ((data.reverse_bits() as u64) << 1);
        self.scan_dr_pause_idle_no_capture(JtagInstruction::Intest, value, 33)?;
        Ok(())
    }

    /// Convert the core from Thumb state to ARM state, per OpenOCD's `arm7tdmi_change_to_arm`.
    ///
    /// The [`arm7_instructions`] sequences are ARM-encoded; a core halted in Thumb state would
    /// misdecode them. Call once right after a halt with `TBIT` set, before any other chain-1
    /// sequence.
    ///
    /// Returns the original `r0` and `pc`, like OpenOCD's version.
    fn change_to_arm(&mut self) -> Result<(u32, u32), Arm7tdmiError> {
        use thumb_instructions::*;

        // Save r0: STR r0,[r0] fetch, decode, execute, then capture (like
        // `arm7tdmi_read_core_regs`).
        self.clock1_no_capture(false, STR_R0_R0)?;
        self.clock1_no_capture(false, NOP)?;
        self.clock1_no_capture(false, NOP)?;
        let r0 = self.clock1(false, NOP)?;

        // r0 = pc, then save it the same way; one extra clock to fetch the MOV first (OpenOCD:
        // MOV, STR, NOP, NOP, data_in).
        self.clock1_no_capture(false, MOV_R0_R15)?;
        self.clock1_no_capture(false, STR_R0_R0)?;
        self.clock1_no_capture(false, NOP)?;
        self.clock1_no_capture(false, NOP)?;
        let pc = self.clock1(false, NOP)?;

        // Switch to ARM via `LDR r0, [pc]` (injecting 0) and `BX r0`, like OpenOCD. Not
        // `BX pc`: in Thumb state its bit 1 depends on the halt address, and entering ARM state
        // at a non-word-aligned PC is unpredictable.
        self.clock1_no_capture(false, LDR_R0_PCREL)?;
        self.clock1_no_capture(false, NOP)?;
        self.clock1_no_capture(false, NOP)?;
        // Nothing fetched: data for `LDR r0, [pc, #0]`.
        self.clock1_no_capture(false, 0)?;
        // Nothing fetched: the loaded data is written to r0.
        self.clock1_no_capture(false, NOP)?;
        // Fetch BX r0; then NOP fetched with BX in decode; NOP fetched with BX in execute.
        self.clock1_no_capture(false, BX_R0)?;
        self.clock1_no_capture(false, NOP)?;
        self.clock1_no_capture(false, NOP)?;

        // MOV r0, r15 was the 4th instruction (+6), and Thumb PC reads as address + 4; OpenOCD
        // subtracts 0xa as well.
        if pc & 1 != 0 {
            // A Thumb `MOV r0, pc` always yields a halfword-aligned value: the core didn't
            // execute the injected Thumb instructions as expected.
            tracing::warn!("Thumb-to-ARM conversion captured an impossible PC {pc:#010x}");
        }
        let pc = pc.wrapping_sub(0xa);

        tracing::debug!("Converted core from Thumb to ARM state (r0=0x{r0:08X}, pc=0x{pc:08X})");
        Ok((r0, pc))
    }

    /// Convert the core to ARM state after a halt known to be in Thumb state, using the known
    /// halt PC instead of [`Self::change_to_arm`]'s captured one.
    ///
    /// Used by `Arm7tdmi::step`'s completion path after [`Self::latch_current_halt`]: all
    /// chain-1 sequences assume ARM state, like after every other halt path
    /// ([`Self::enter_debug_state`]). `known_good_pc` is the address the step
    /// watchpoint was armed on.
    pub(crate) fn convert_to_arm_after_thumb_step(
        &mut self,
        known_good_pc: u32,
    ) -> Result<(), Arm7tdmiError> {
        let (r0, _uncorrected_pc) = self.change_to_arm()?;
        self.write_core_register_unchecked(0, r0)?;
        self.write_core_register_unchecked(15, known_good_pc)?;
        Ok(())
    }

    /// Run a register-only (non memory-accessing) instruction to completion: fetch, decode,
    /// execute. Safe to use at debug (TCK) speed since no external bus transaction occurs.
    fn execute_simple(&mut self, instruction: u32) -> Result<(), Arm7tdmiError> {
        self.clock1_no_capture(false, instruction)?;
        self.clock1_no_capture(false, arm7_instructions::NOP)?;
        self.clock1_no_capture(false, arm7_instructions::NOP)?;
        Ok(())
    }

    /// Run a data-transfer instruction and capture the data-bus value of its data cycle (used
    /// to read registers).
    ///
    /// Debug-speed only, no real bus transaction; for real memory accesses see
    /// [`Self::system_speed_access`].
    fn execute_data_transfer(&mut self, instruction: u32) -> Result<u32, Arm7tdmiError> {
        // Fetch + 2 NOPs + capture, like OpenOCD's `arm7tdmi_read_core_regs`.
        self.clock1_no_capture(false, instruction)?; // fetch
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // decode
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // execute / address calculation
        self.clock1(false, arm7_instructions::NOP) // data-transfer cycle
    }

    /// Load an arbitrary constant into `register` by injecting it on the data bus during
    /// `LDMIA R0, {register}`'s data cycle; R0's value is irrelevant. Like OpenOCD's
    /// `arm7tdmi_write_core_regs` with a single-bit mask.
    fn load_immediate(&mut self, register: u8, value: u32) -> Result<(), Arm7tdmiError> {
        let instr = arm7_instructions::LOAD_MULTIPLE_R0 | (1u32 << register);
        self.clock1_no_capture(false, instr)?; // fetch LDMIA R0, {register}
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // decode
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // execute (1): address setup
        self.clock1_no_capture(false, value)?; // data-transfer cycle: inject `value`
        // Trailing clock to commit the value, like `arm7tdmi_write_core_regs`.
        self.clock1_no_capture(false, arm7_instructions::NOP)?;
        Ok(())
    }

    /// Write R15 (PC) via `LDMIA R0, {R15}`: [`Self::load_immediate`] plus three extra NOPs,
    /// matching OpenOCD's `arm7tdmi_write_pc` clock for clock, as needed before
    /// [`Self::branch_resume`].
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

    /// Clock a branch back to the PC just written by [`Self::write_pc`], with `BREAKPT` set on
    /// the preceding clock, like OpenOCD's `arm7tdmi_branch_resume`.
    ///
    /// Must directly follow `write_pc` (see [`arm7_instructions::BRANCH_BACK_TO_PC`]), which is
    /// why `resume()` redoes `write_pc` right before calling this.
    fn branch_resume(&mut self) -> Result<(), Arm7tdmiError> {
        self.clock1_no_capture(true, arm7_instructions::NOP)?;
        self.clock1_no_capture(false, arm7_instructions::BRANCH_BACK_TO_PC)?;
        Ok(())
    }

    /// Resume into `pc` in Thumb state (see
    /// [`Arm7tdmiDebugInterfaceState::pending_resume_thumb`]).
    ///
    /// Ported clock-for-clock from OpenOCD's `arm7tdmi_branch_resume_thumb`: load `pc | 1` into
    /// R0 and `BX R0`, which sets PC and T atomically. Mode, flags and I/F are untouched. Not an
    /// SPSR write plus `MOVS PC, LR`: OpenOCD never changes T via CPSR/SPSR writes
    /// (`arm7_9_restore_context` masks it out).
    ///
    /// R0 is restored after the `BX` with a Thumb-encoded `LDR`, since the core is in Thumb
    /// state by then. The core must be in ARM state on entry.
    fn branch_resume_thumb_aware(&mut self, pc: u32) -> Result<(), Arm7tdmiError> {
        let saved_r0 = self.read_core_register(0)?;

        // `LDMIA r0,{r0}` injecting `pc | 1`, as in OpenOCD.
        self.load_immediate(0, pc | 1)?;

        // `BX r0`: a single clock, as in OpenOCD, not `execute_simple`'s fetch+2 NOPs.
        self.clock1_no_capture(false, arm7_instructions::BX_R0)?;

        // Let the transition settle with OpenOCD's interleaved chain-2 status reads and NOP
        // clocks (chain 2 does not depend on the decode state).
        self.read_debug_status()?;
        self.clock1_no_capture(false, arm7_instructions::NOP)?;
        self.read_debug_status()?;
        self.clock1_no_capture(false, arm7_instructions::NOP)?;
        self.read_debug_status()?;

        // Restore r0 with a Thumb `LDR`, the core now decodes Thumb.
        self.clock1_no_capture(false, thumb_instructions::LDR_R0_PCREL)?;
        self.clock1_no_capture(false, thumb_instructions::NOP)?;
        self.clock1_no_capture(false, thumb_instructions::NOP)?;
        self.clock1_no_capture(false, saved_r0)?;
        self.clock1_no_capture(false, thumb_instructions::NOP)?;

        self.clock1_no_capture(false, thumb_instructions::NOP)?;
        self.clock1_no_capture(false, thumb_instructions::NOP)?;
        self.read_debug_status()?;

        // Thumb branch back to `pc` with BREAKPT on the preceding clock, handing off to
        // `resume()`'s RESTART. Offset calibrated like `BRANCH_BACK_TO_PC`; matches OpenOCD's
        // trailing `ARMV4_5_T_NOP` (bp=1) + `ARMV4_5_T_B(0x7f8)`.
        self.clock1_no_capture(true, thumb_instructions::NOP)?;
        self.clock1_no_capture(false, thumb_instructions::BRANCH_BACK_TO_PC)?;
        Ok(())
    }

    /// Select `instruction` and go straight to Run-Test/Idle without a DR shift, like OpenOCD's
    /// `arm_jtag_set_instr(tap, N, NULL, TAP_IDLE)`. Used for RESTART.
    ///
    /// Includes one extra TCK in Run-Test/Idle (OpenOCD's trace ends `UpdIR->RTI->RTI`): without
    /// it RESTART does not take hold on this silicon and the next JTAG operation stops the core.
    fn select_ir_idle(&mut self, instruction: JtagInstruction) -> Result<(), Arm7tdmiError> {
        let mut batch = JtagBatch::new();
        let ir_seq = BitSequence::from_u64(IR_LEN, instruction as u64);
        self.probe.shift_ir(&mut batch, &ir_seq);
        self.probe.run_test_idle(&mut batch, 1);
        self.probe
            .run(batch)
            .inspect_err(|_| self.forget_tap_selection())?;
        self.current_instruction = Some(instruction);
        Ok(())
    }

    /// Let the instruction queued for its data cycle run at system speed against the real bus,
    /// so flash/peripheral wait states are honored.
    ///
    /// Per ARM DDI 0029G B.8.2, BREAKPT must be set on the clock before the instruction that
    /// should run at system speed (see [`Self::system_speed_transfer`]). RESTART then lets the
    /// core run that access and re-enter debug state, signalled by DBGACK and SYSCOMP.
    ///
    /// Callers must clear `debug_control::STICKY_HALT` before the preceding chain-1 clocking
    /// (see [`Self::with_sticky_halt_cleared`]), not here: that would select chain 2 between
    /// the chain-1 clocks and RESTART and lose the pending access. OpenOCD's
    /// `arm7_9_read_memory`/`arm7_9_write_memory` also clear it once before the whole sequence.
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
    /// R0-R15 are read via `STMIA R0, {reg}`, like OpenOCD's `arm7tdmi_read_core_regs`, with
    /// the value captured from the data bus. This does perform a real store cycle to R0's
    /// address; ARM7TDMI has no side-effect-free register access. CPSR uses a different
    /// sequence (see `read_core_register_unchecked`).
    pub fn read_core_register(&mut self, register: u8) -> Result<u32, Arm7tdmiError> {
        self.ensure_halted()?;
        // Serve PC from the halt cache (see `cache_resume_pc`): every chain-1 capture advances
        // the pipeline, so a fresh PC read would drift from the value just established (e.g. a
        // GDB `set $pc` / `info registers` round trip). Only `enter_debug_state` needs a fresh
        // capture, via `read_core_register_unchecked`.
        if register == 15
            && let Some(pc) = self.cached_halt_pc()
        {
            return Ok(pc);
        }
        self.read_core_register_unchecked(register)
    }

    /// Like [`Self::read_core_register`], without the halted check, for callers establishing
    /// the halt ([`Self::enter_debug_state`]); the checked version would recurse into
    /// [`Self::halt`] via [`Self::ensure_halted`].
    fn read_core_register_unchecked(&mut self, register: u8) -> Result<u32, Arm7tdmiError> {
        match register {
            0..=15 => {
                let instr = arm7_instructions::STORE_MULTIPLE_R0 | (1u32 << register);
                let value = self.execute_data_transfer(instr)?;
                if register == 15 {
                    // `STR PC` on ARMv4T stores PC + 12.
                    Ok(value.wrapping_sub(12))
                } else {
                    Ok(value)
                }
            }
            16 => {
                // `MRS R0, CPSR` directly followed by `STR R0, [R15]`, like OpenOCD's
                // `arm7tdmi_read_xpsr` (see `STR_R0_R15`). This clobbers the real R0, so it is
                // saved and restored. Requires ARM state.
                let saved_r0 = self.read_core_register_unchecked(0)?;
                let cpsr = self.capture_cpsr_clobbering_r0()?;
                self.write_core_register_unchecked(0, saved_r0)?;

                // The core is in ARM state while halted; report the T bit it will resume with.
                if self.state.pending_resume_thumb {
                    Ok(cpsr | CPSR_TBIT)
                } else {
                    Ok(cpsr)
                }
            }
            _ => Err(Arm7tdmiError::InvalidRegister(register)),
        }
    }

    /// Read several of R0-R15 with one `STMIA` per chunk instead of one per register.
    ///
    /// Like OpenOCD's `arm7tdmi_read_core_regs`: fetch `STMIA R0, {mask}` + two NOPs, then one
    /// capture per set bit in ascending register order. This pays the fetch/decode/execute
    /// overhead (and pipeline advance, see [`crate::architecture::arm7`]) once per chunk rather
    /// than per register. R15 gets the same `- 12` adjustment; CPSR is not included.
    ///
    /// Only the first `Self::MAX_REGISTERS_PER_TRANSFER` data cycles of one `STMIA` capture
    /// valid data at debug speed (later ones return a stale value, a silicon limit, not an
    /// adapter one), so requests are split into chunks of that size.
    pub fn read_core_registers(&mut self, mask: u16) -> Result<Vec<(u8, u32)>, Arm7tdmiError> {
        self.ensure_halted()?;

        let mut results = Vec::with_capacity(mask.count_ones() as usize);
        let mut remaining = mask;

        // Serve PC from the halt cache, as in `read_core_register`.
        if remaining & (1 << 15) != 0
            && let Some(pc) = self.cached_halt_pc()
        {
            results.push((15, pc));
            remaining &= !(1 << 15);
        }

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

    /// Maximum registers per `STMIA` capture in [`Self::read_core_registers`].
    const MAX_REGISTERS_PER_TRANSFER: u8 = 4;

    /// Read the current mode's banked SPSR via `MRS R0, SPSR` + `STR R0, [R15]`, like the CPSR
    /// read. Returns the raw value (SPSR's T bit is real). Clobbers R0.
    fn read_spsr(&mut self) -> Result<u32, Arm7tdmiError> {
        self.clock1_no_capture(false, arm7_instructions::MRS_R0_SPSR)?; // fetch MRS
        self.clock1_no_capture(false, arm7_instructions::STR_R0_R15)?; // fetch STR (MRS decode)
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // MRS execute, STR decode
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // STR execute (1st cycle)
        self.clock1(false, arm7_instructions::NOP) // STR execute (2nd cycle) / capture
    }

    /// Read the current mode's SPSR, preserving R0 (see [`Self::read_spsr`]).
    pub(crate) fn read_spsr_preserving_r0(&mut self) -> Result<u32, Arm7tdmiError> {
        self.ensure_halted()?;
        let saved_r0 = self.read_core_register(0)?;
        let spsr = self.read_spsr()?;
        self.write_core_register_unchecked(0, saved_r0)?;
        Ok(spsr)
    }

    /// Capture the core's real CPSR via `MRS R0, CPSR` + `STR R0, [R15]` (see the register-16
    /// case of [`Self::read_core_register_unchecked`]). Clobbers R0 - the caller restores it.
    fn capture_cpsr_clobbering_r0(&mut self) -> Result<u32, Arm7tdmiError> {
        self.clock1_no_capture(false, arm7_instructions::MRS_R0_CPSR)?; // fetch MRS
        self.clock1_no_capture(false, arm7_instructions::STR_R0_R15)?; // fetch STR (MRS decode)
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // MRS execute, STR decode
        self.clock1_no_capture(false, arm7_instructions::NOP)?; // STR execute (1st cycle)
        self.clock1(false, arm7_instructions::NOP) // STR execute (2nd cycle) / capture
    }

    /// Write CPSR via four `MSR CPSR_<field>, #imm` instructions, as given (the caller keeps T
    /// clear - see [`Self::write_core_register`]).
    fn write_cpsr_raw(&mut self, value: u32) -> Result<(), Arm7tdmiError> {
        // (field_mask, rotate_imm, byte)
        let fields = [
            (0b0001u32, 0u32, value & 0xFF),
            (0b0010, 12, (value >> 8) & 0xFF),
            (0b0100, 8, (value >> 16) & 0xFF),
            (0b1000, 4, (value >> 24) & 0xFF),
        ];
        for (mask, rotate, imm8) in fields {
            let instr = arm7_instructions::MSR_CPSR_IMM | (mask << 16) | (rotate << 8) | imm8;
            self.execute_simple(instr)?;
        }
        Ok(())
    }

    /// The core's real CPSR for the current halt, read once and cached. Clobbers R0 on a cache
    /// miss - only for callers that already saved it (the memory accessors).
    fn halt_cpsr_clobbering_r0(&mut self) -> Result<u32, Arm7tdmiError> {
        if let Some(cpsr) = self.state.halt_cpsr {
            return Ok(cpsr);
        }
        let cpsr = self.capture_cpsr_clobbering_r0()?;
        self.state.halt_cpsr = Some(cpsr);
        Ok(cpsr)
    }

    /// Check whether the system-speed access just performed raised a data abort, and undo its
    /// mode change if so. `before` is the CPSR from before the access. Clobbers R0 - only for
    /// the memory accessors, which restore it afterwards.
    ///
    /// An abort switches the core to Abort mode (and masks IRQs), so every later register
    /// access and the eventual resume would use the wrong register bank. Like OpenOCD's
    /// `arm7_9_read_memory`, this restores CPSR and reports the abort; R14_abt/SPSR_abt keep the
    /// values the abort gave them. An abort raised while the core was already halted in Abort
    /// mode can't be told apart from a normal access and isn't detected.
    fn check_data_abort(&mut self, before: u32, address: u32) -> Result<(), Arm7tdmiError> {
        const MODE_MASK: u32 = 0x1F;
        const MODE_ABORT: u32 = 0x17;
        if before & MODE_MASK == MODE_ABORT {
            return Ok(());
        }
        let after = self.capture_cpsr_clobbering_r0()?;
        if after & MODE_MASK != MODE_ABORT {
            return Ok(());
        }
        tracing::warn!("memory access at {address:#010x} raised a data abort");
        self.write_cpsr_raw(before)?;
        Err(Arm7tdmiError::DataAbort { address })
    }

    /// Return from the current exception like a normal handler: PC = LR, CPSR = SPSR. Used to
    /// resume from a semihosting SVC vector catch (see `Arm7tdmi::run`).
    ///
    /// Does not execute `MOVS PC, LR`: changing T via a CPSR restore is not how OpenOCD resumes
    /// into Thumb (only `BX` is reliable), and it would bypass `pending_resume_pc`. Instead SPSR
    /// and LR are read and fed into [`Arm7tdmiDebugInterfaceState::pending_resume_pc`] /
    /// `pending_resume_thumb` for the `BX`-based [`Self::resume`] path; the other SPSR bits are
    /// written via the MSR-based CPSR write with T kept clear.
    pub(crate) fn return_from_exception(&mut self) -> Result<(), Arm7tdmiError> {
        self.ensure_halted()?;

        // `read_spsr` clobbers R0, which may already hold the semihosting return value (written
        // by the RPC layer before this call), so save and restore it.
        let saved_r0 = self.read_core_register(0)?;
        let spsr = self.read_spsr()?;
        let lr = self.read_core_register(14)?;

        // Records SPSR's T bit in `pending_resume_thumb` (see `write_core_register_unchecked`).
        self.write_core_register_unchecked(16, spsr)?;
        self.write_core_register_unchecked(0, saved_r0)?;

        self.cache_resume_pc(lr);
        self.record_halt_state_if_entered()
    }

    /// Write a core register (R0-R15, or 16 for CPSR).
    ///
    /// R0-R14 are written via `Self::load_immediate`. R15 goes through `Self::write_pc` and is
    /// cached in `pending_resume_pc` for `resume()` to redo right before the branch. A PC write
    /// does not change the instruction set.
    ///
    /// CPSR is written a byte at a time via four `MSR CPSR_<field>, #imm`. The T bit is never
    /// written through MSR, since the core must stay in ARM state for the injected instructions
    /// (OpenOCD masks it out too); it is recorded in `pending_resume_thumb` instead, so
    /// `resume()` enters the requested state and CPSR reads report it.
    pub fn write_core_register(&mut self, register: u8, value: u32) -> Result<(), Arm7tdmiError> {
        self.ensure_halted()?;
        self.write_core_register_unchecked(register, value)
    }

    /// Like [`Self::write_core_register`], without the halted check, for
    /// [`Self::enter_debug_state`], which just confirmed DBGACK and avoids more chain-2 status
    /// reads in the burst right after a halt, where this silicon is less reliable.
    fn write_core_register_unchecked(
        &mut self,
        register: u8,
        value: u32,
    ) -> Result<(), Arm7tdmiError> {
        match register {
            15 => {
                self.write_pc(value)?;
                self.state.pending_resume_pc = Some(value);
                // `pending_resume_thumb` is left alone (see the doc comment). Callers jumping to
                // ARM code from a possibly-Thumb halt clear CPSR.T explicitly.
                self.record_halt_state_if_entered()
            }
            0..=14 => {
                self.load_immediate(register, value)?;
                Ok(())
            }
            16 => {
                self.state.pending_resume_thumb = value & CPSR_TBIT != 0;
                let value = value & !CPSR_TBIT;
                self.state.halt_cpsr = None;
                self.write_cpsr_raw(value)?;
                self.record_halt_state_if_entered()
            }
            _ => Err(Arm7tdmiError::InvalidRegister(register)),
        }
    }

    /// Restore registers the memory accessors clobbered, attempting every one even if an
    /// earlier write fails, and report the first failure.
    fn restore_registers(&mut self, saved: &[(u8, u32)]) -> Result<(), Arm7tdmiError> {
        let mut result = Ok(());
        for &(register, value) in saved {
            let restored = self.write_core_register_unchecked(register, value);
            if result.is_ok() {
                result = restored;
            }
        }
        result
    }

    /// Run `access` (one or more system-speed accesses) with `STICKY_HALT` cleared, as
    /// `Self::system_speed_access` requires, and always set it again afterwards, also on error
    /// (RESTART may already have released the core).
    fn with_sticky_halt_cleared<T>(
        &mut self,
        access: impl FnOnce(&mut Self) -> Result<T, Arm7tdmiError>,
    ) -> Result<T, Arm7tdmiError> {
        self.write_debug_control(debug_control::INTDIS)?;
        let result = access(self);
        // STICKY_HALT|INTDIS, as in OpenOCD's trace on this board (`0x00000005`).
        self.write_debug_control(debug_control::STICKY_HALT | debug_control::INTDIS)?;
        result
    }

    /// Queue `instruction` (an `LDMIA`/`STMIA R0!, {..}`) for system-speed execution and run it.
    ///
    /// Like OpenOCD's `arm7tdmi_load_word_regs`/`arm7tdmi_store_word_regs`: BREAKPT goes on a
    /// NOP before the instruction's fetch; the instruction is only fetched at debug speed and
    /// RESTART runs the rest at system speed.
    fn system_speed_transfer(&mut self, instruction: u32) -> Result<(), Arm7tdmiError> {
        self.clock1_no_capture(false, arm7_instructions::NOP)?;
        self.clock1_no_capture(true, arm7_instructions::NOP)?;
        self.clock1_no_capture(false, instruction)?;
        self.system_speed_access()
    }

    /// Read memory at the given address (must be word-aligned).
    ///
    /// The load runs at system speed (see `Self::system_speed_access`) to reach the real bus.
    /// `LDMIA R0!, {R1}` overwrites the real R0/R1, which may hold live values of the halted
    /// program, so they are saved and restored.
    ///
    /// Returns [`Arm7tdmiError::DataAbort`] if the access aborted (see `check_data_abort`).
    pub fn read_memory_32(&mut self, address: u32) -> Result<u32, Arm7tdmiError> {
        self.ensure_halted()?;
        let saved = [
            (0, self.read_core_register(0)?),
            (1, self.read_core_register(1)?),
        ];

        let result = (|| {
            let before = self.halt_cpsr_clobbering_r0()?;
            self.with_sticky_halt_cleared(|this| {
                this.load_immediate(0, address)?;
                this.system_speed_transfer(arm7_instructions::LOAD_MULTIPLE_R0_WRITEBACK | (1 << 1))
            })?;
            self.check_data_abort(before, address)?;
            self.read_core_register(1)
        })();

        let restored = self.restore_registers(&saved);
        let value = result?;
        restored?;
        Ok(value)
    }

    /// Write memory at the given address (must be word-aligned). Saves and restores R0/R1 and
    /// detects data aborts like [`Self::read_memory_32`].
    pub fn write_memory_32(&mut self, address: u32, value: u32) -> Result<(), Arm7tdmiError> {
        self.ensure_halted()?;
        let saved = [
            (0, self.read_core_register(0)?),
            (1, self.read_core_register(1)?),
        ];

        let result = (|| {
            let before = self.halt_cpsr_clobbering_r0()?;
            self.with_sticky_halt_cleared(|this| {
                this.load_immediate(0, address)?;
                this.load_immediate(1, value)?;
                this.system_speed_transfer(
                    arm7_instructions::STORE_MULTIPLE_R0_WRITEBACK | (1 << 1),
                )
            })?;
            self.check_data_abort(before, address)
        })();

        let restored = self.restore_registers(&saved);
        result?;
        restored
    }

    /// Write consecutive words starting at `address`, as a single burst.
    ///
    /// Clears `STICKY_HALT` once for the whole burst and loads R0 once, relying on the
    /// `STMIA R0!, {R1}` write-back, like OpenOCD's `arm7_9_write_memory` (which also batches up
    /// to 14 registers per access; not replicated here).
    ///
    /// Toggling `STICKY_HALT` per word (a loop over `write_memory_32`) was observed to drift the
    /// real PC by tens of KB over a 256-byte burst, although each call reported success.
    ///
    /// A data abort is detected once, after the whole burst.
    pub fn write_memory_32_bulk(
        &mut self,
        address: u32,
        values: &[u32],
    ) -> Result<(), Arm7tdmiError> {
        if values.is_empty() {
            return Ok(());
        }

        self.ensure_halted()?;
        let saved = [
            (0, self.read_core_register(0)?),
            (1, self.read_core_register(1)?),
        ];

        let result = (|| {
            let before = self.halt_cpsr_clobbering_r0()?;
            self.with_sticky_halt_cleared(|this| {
                this.load_immediate(0, address)?;
                for &value in values {
                    this.load_immediate(1, value)?;
                    this.system_speed_transfer(
                        arm7_instructions::STORE_MULTIPLE_R0_WRITEBACK | (1 << 1),
                    )?;
                }
                Ok(())
            })?;
            self.check_data_abort(before, address)
        })();

        let restored = self.restore_registers(&saved);
        result?;
        restored
    }

    /// Read consecutive words starting at `address`, as a burst.
    ///
    /// Queues one system-speed `LDMIA R0!, {R1..R(chunk)}` per up to
    /// `Self::MAX_REGISTERS_PER_TRANSFER` words (one `STICKY_HALT` toggle per chunk, see
    /// [`Self::write_memory_32_bulk`]) and captures the loaded registers via
    /// [`Self::read_core_registers`].
    ///
    /// R0-R4 are saved and restored around the whole burst; a data abort is detected once,
    /// after it.
    pub fn read_memory_32_bulk(
        &mut self,
        address: u32,
        count: usize,
    ) -> Result<Vec<u32>, Arm7tdmiError> {
        if count == 0 {
            return Ok(Vec::new());
        }

        self.ensure_halted()?;
        const SAVE_MASK: u16 = 0b11111; // R0..R4
        let saved: Vec<(u8, u32)> = self.read_core_registers(SAVE_MASK)?;

        let result = (|| {
            let before = self.halt_cpsr_clobbering_r0()?;
            let mut results = Vec::with_capacity(count);
            let mut remaining = count;
            let mut addr = address;
            while remaining != 0 {
                let chunk = remaining.min(Self::MAX_REGISTERS_PER_TRANSFER as usize);
                // Registers R1..=R(chunk) - R0 is the LDMIA base register.
                let reg_mask: u32 = (1u32 << (chunk + 1)) - 2;

                self.with_sticky_halt_cleared(|this| {
                    this.load_immediate(0, addr)?;
                    this.system_speed_transfer(
                        arm7_instructions::LOAD_MULTIPLE_R0_WRITEBACK | reg_mask,
                    )
                })?;

                // `read_core_registers` returns a single chunk (`reg_mask` never spans more
                // than `MAX_REGISTERS_PER_TRANSFER` bits) in ascending register order.
                for (_, value) in self.read_core_registers(reg_mask as u16)? {
                    results.push(value);
                }

                addr = addr.wrapping_add((chunk as u32) * 4);
                remaining -= chunk;
            }
            self.check_data_abort(before, address)?;
            Ok::<_, Arm7tdmiError>(results)
        })();

        let restored = self.restore_registers(&saved);
        let values = result?;
        restored?;
        Ok(values)
    }

    /// Halt the core by setting DBGRQ in the Debug Control Register
    pub fn halt(&mut self) -> Result<(), Arm7tdmiError> {
        tracing::info!("Halting ARM7TDMI core");

        // The status register can't tell why the core is halted. If it already is (e.g. an
        // armed breakpoint hit), asserting DBGRQ would apply the wrong PC correction, so treat
        // it as a watchpoint halt, as `status()` does.
        if self.is_halted()? {
            tracing::debug!(
                "halt(): core was already halted before DBGRQ was asserted - treating as a \
                 watchpoint-style halt instead"
            );
            return self.latch_watchpoint_halt();
        }

        self.write_debug_control(debug_control::DBGRQ)?;

        let start = std::time::Instant::now();
        // DBGRQ can take several seconds to take effect on this silicon.
        let timeout = std::time::Duration::from_secs(3);

        while start.elapsed() < timeout {
            let status = self.read_debug_status()?;
            if status & debug_status::DBGACK != 0 {
                self.state.in_debug_state = true;
                // Latch the halt with `STICKY_HALT` and release DBGRQ (OpenOCD's trace on this
                // board: `0x00000005`).
                self.write_debug_control(debug_control::STICKY_HALT | debug_control::INTDIS)?;
                // Written twice like OpenOCD, whose `arm7_9_clear_halt` rewrites the register
                // after `arm7_9_debug_entry` for a DBGRQ halt.
                self.write_debug_control(debug_control::STICKY_HALT | debug_control::INTDIS)?;
                tracing::debug!("Core halted successfully");
                self.state.halt_requested = true;
                self.enter_debug_state(status, true)?;
                return Ok(());
            }
            // No sleep between polls, like OpenOCD's `arm7_9_poll` loop.
        }

        // Don't leave DBGRQ asserted: the core would halt later, unannounced, and that halt
        // would be mistaken for a breakpoint hit (and get the breakpoint PC correction).
        self.write_debug_control(0)?;
        Err(Arm7tdmiError::Timeout)
    }

    /// Finish entering debug state after a halt was latched (`STICKY_HALT` set), for both
    /// [`Self::halt`] and [`Self::latch_watchpoint_halt`].
    ///
    /// Converts a Thumb halt to ARM state, since all chain-1 sequences are ARM-encoded.
    /// `status` is the Debug Status Register value read when DBGACK was seen, so its `TBIT`
    /// reflects the halt-time state. `dbgrq` is `true` for a DBGRQ halt (not synchronized to an
    /// instruction boundary) and `false` for a breakpoint/watchpoint match; the PC corrections
    /// differ.
    fn enter_debug_state(&mut self, status: u32, dbgrq: bool) -> Result<(), Arm7tdmiError> {
        if self.state.debug_entry_done {
            tracing::debug!("enter_debug_state(): already done for this halt, skipping");
            return Ok(());
        }
        if status & debug_status::TBIT != 0 {
            tracing::debug!("Halted in Thumb state, converting to ARM");
            let (r0, pc) = self.change_to_arm()?;
            // Thumb PC correction on top of change_to_arm's `-0xa`: DBGRQ `-4`, like OpenOCD's
            // `arm7_9_debug_entry` (`dbgreq_adjust_pc * 2`, `dbgreq_adjust_pc = 2`); a
            // breakpoint at a Thumb function entry reports 6 bytes past the address, so `-6`.
            let pc = if dbgrq {
                pc.wrapping_sub(4)
            } else {
                pc.wrapping_sub(6)
            };
            // change_to_arm left PC at 0; restore the original r0/pc before any further chain-1
            // operation, like OpenOCD's `arm7_9_debug_entry`.
            self.write_core_register_unchecked(0, r0)?;
            self.write_core_register_unchecked(15, pc)?;
            // Resume via `branch_resume_thumb_aware`.
            self.state.pending_resume_thumb = true;
        } else {
            self.state.pending_resume_thumb = false;
            // Cache the halt PC for a bare `resume()`: re-reading it later, after other chain-1
            // operations, would return a drifted value.
            //
            // On top of the `-12` STM capture lag: DBGRQ needs `-8` (OpenOCD's
            // `dbgreq_adjust_pc * 4`), a fetch watchpoint match reports a further `+12`.
            //
            // The unchecked read avoids recursing into `ensure_halted` -> `halt`. DBGACK can
            // briefly read back cleared right after a halt, so re-latch and retry the capture
            // (bounded) if it has dropped.
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
        self.state.debug_entry_done = true;
        self.record_halt_state()
    }

    /// Resume the core by clearing DBGRQ.
    /// Before releasing the core, redirects the pipeline with `Self::write_pc` directly
    /// followed by `Self::branch_resume`; an injected PC alone leaves stale prefetched
    /// instructions that RESTART would execute. Like OpenOCD's `arm7_9_resume`, which always
    /// rewrites PC and calls `branch_resume` before touching debug control or RESTART.
    pub fn resume(&mut self) -> Result<(), Arm7tdmiError> {
        self.resume_inner(false)
    }

    /// Like [`Self::resume`], but keeps IRQ/FIQ masked (`INTDIS`) while the core runs, so a
    /// single step executes the stepped instruction instead of taking a pending interrupt.
    pub(crate) fn resume_for_step(&mut self) -> Result<(), Arm7tdmiError> {
        self.resume_inner(true)
    }

    fn resume_inner(&mut self, keep_interrupts_disabled: bool) -> Result<(), Arm7tdmiError> {
        tracing::info!("Resuming ARM7TDMI core");

        // Use the explicitly written PC if any, else the current PC. Only cleared below once
        // the core is released, so a failed resume doesn't lose it.
        let pc = match self.state.pending_resume_pc {
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
        // A Thumb halt needs a `BX` (see `branch_resume_thumb_aware`).
        match self.state.pending_resume_thumb {
            true => {
                tracing::debug!("resume() restoring Thumb state via BX, pc={:#x}", pc);
                self.branch_resume_thumb_aware(pc)?;
            }
            false => {
                self.write_pc(pc)?;
                self.branch_resume()?;
            }
        }

        // Clear DBGRQ (and, unless stepping, INTDIS) in the Debug Control Register.
        self.write_debug_control(if keep_interrupts_disabled {
            debug_control::INTDIS
        } else {
            0
        })?;

        // See `select_ir_idle`.
        self.select_ir_idle(JtagInstruction::Restart)?;

        self.state.in_debug_state = false;
        self.state.pending_resume_pc = None;
        self.state.pending_resume_thumb = false;
        self.state.debug_entry_done = false;
        self.state.halt_cpsr = None;
        self.state.halt_requested = false;
        // Only now that the core is released; a failed resume leaves the record valid.
        self.clear_halt_record()?;
        tracing::debug!("Core resumed");
        Ok(())
    }

    /// Cache `pc` as [`Arm7tdmiDebugInterfaceState::pending_resume_pc`], so a later bare
    /// `resume()` and PC reads reuse it instead of a fresh chain-1 capture (which advances the
    /// pipeline, see [`crate::architecture::arm7`]).
    pub(crate) fn cache_resume_pc(&mut self, pc: u32) {
        self.state.pending_resume_pc = Some(pc);
    }

    /// The cached halt-time PC (see [`Self::cache_resume_pc`]), without a chain-1 read. Any
    /// later capture would already see a pipeline advanced by earlier reads, so "where did the
    /// core halt" must use this value.
    pub(crate) fn cached_halt_pc(&self) -> Option<u32> {
        self.state.pending_resume_pc
    }

    /// Whether the current halt was requested by this debugger, see
    /// [`Arm7tdmiDebugInterfaceState::halt_requested`].
    pub(crate) fn halt_was_requested(&self) -> bool {
        self.state.halt_requested
    }

    /// Set [`Arm7tdmiDebugInterfaceState::pending_resume_thumb`] directly, for `step()`, which
    /// determines the instruction set the core halted in itself.
    pub(crate) fn set_pending_resume_thumb(&mut self, thumb: bool) {
        self.state.pending_resume_thumb = thumb;
    }

    /// Record that debug-state entry for the current halt is complete, for a halt path that
    /// does its own entry work instead of going through `enter_debug_state` (`step()`) - see
    /// [`Arm7tdmiDebugInterfaceState::debug_entry_done`].
    pub(crate) fn mark_debug_entry_done(&mut self) -> Result<(), Arm7tdmiError> {
        self.state.debug_entry_done = true;
        self.record_halt_state()
    }

    /// Record the current halt's resume PC and instruction set on the target, in two
    /// EmbeddedICE registers (see [`HALT_RECORD_PC`]).
    ///
    /// Debug-state entry converts a Thumb halt to ARM state, so only this process knows the
    /// core must resume in Thumb. If it exits leaving the core halted, the next session would
    /// resume Thumb code in ARM state; the record lets it take over the halt instead (see
    /// [`Self::adopt_recorded_halt`]). Cleared when the core runs.
    fn record_halt_state(&mut self) -> Result<(), Arm7tdmiError> {
        let Some(pc) = self.state.pending_resume_pc else {
            return self.clear_halt_record();
        };
        self.write_ice_register(HALT_RECORD_PC, pc)?;
        let tag = HALT_RECORD_MAGIC | self.state.pending_resume_thumb as u32;
        self.write_ice_register(HALT_RECORD_TAG, tag)
    }

    /// [`Self::record_halt_state`], if debug-state entry for the current halt is complete (a
    /// register write during the entry itself is recorded at its end).
    fn record_halt_state_if_entered(&mut self) -> Result<(), Arm7tdmiError> {
        if self.state.debug_entry_done {
            self.record_halt_state()?;
        }
        Ok(())
    }

    fn clear_halt_record(&mut self) -> Result<(), Arm7tdmiError> {
        self.write_ice_register(HALT_RECORD_TAG, 0)
    }

    /// Drop a halt record that can't describe the current halt (see `Arm7tdmi::new`).
    pub(crate) fn discard_halt_record(&mut self) -> Result<(), Arm7tdmiError> {
        if self.read_ice_register(HALT_RECORD_TAG)? & !1 == HALT_RECORD_MAGIC {
            tracing::debug!("Discarding a stale halt record: the core is halted in Thumb state");
            self.clear_halt_record()?;
        }
        Ok(())
    }

    /// Take over a halt a previous session left behind, from its halt record (see
    /// [`Self::record_halt_state`]). Returns `false` if there is no record. Only for a core
    /// found halted at attach.
    pub(crate) fn adopt_recorded_halt(&mut self) -> Result<bool, Arm7tdmiError> {
        let tag = self.read_ice_register(HALT_RECORD_TAG)?;
        if tag & !1 != HALT_RECORD_MAGIC {
            return Ok(false);
        }
        let pc = self.read_ice_register(HALT_RECORD_PC)?;
        self.state.pending_resume_pc = Some(pc);
        self.state.pending_resume_thumb = tag & 1 != 0;
        self.state.debug_entry_done = true;
        self.state.in_debug_state = true;
        tracing::debug!(
            "Adopted a halt left by a previous session: pc={pc:#010x}, thumb={}",
            tag & 1 != 0
        );
        Ok(true)
    }

    /// Check if core is halted by reading Debug Status Register.
    ///
    /// If halted, also latches the halt (`STICKY_HALT`/`INTDIS` set, `DBGRQ` cleared), the
    /// first step of OpenOCD's `arm7_9_debug_entry` for every halt reason. Without it a
    /// watchpoint halt (e.g. the flash algorithm's completion breakpoint) can briefly report
    /// DBGACK and then resume on its own, so callers' chain-1 accesses would go nowhere.
    pub fn is_halted(&mut self) -> Result<bool, Arm7tdmiError> {
        let status = self.read_debug_status()?;
        let mut halted = status & debug_status::DBGACK != 0;
        if !halted && self.state.debug_entry_done {
            // Confirm with two more reads before forgetting the debug-state entry: redoing it on
            // a still-halted core that is already in ARM state would go wrong.
            for _ in 0..2 {
                if self.read_debug_status()? & debug_status::DBGACK != 0 {
                    halted = true;
                    break;
                }
            }
        }
        self.state.in_debug_state = halted;
        if !halted && self.state.debug_entry_done {
            // The core runs without `resume()` (a halt that didn't stick): the record is stale.
            self.clear_halt_record()?;
        }
        if !halted {
            self.state.halt_requested = false;
        }
        if !halted {
            // Whatever halts the core next needs its own debug-state entry.
            self.state.debug_entry_done = false;
            self.state.halt_cpsr = None;
        }
        Ok(halted)
    }

    /// Precondition for register/memory access: errors with [`Arm7tdmiError::CoreNotHalted`] if
    /// the core isn't halted, after one re-halt attempt.
    ///
    /// A halt can fail to stick on this hardware, notably right after the flash algorithm ran
    /// (a `read` right after `download`): DBGACK reads cleared a moment later. Re-halting once
    /// recovers this.
    fn ensure_halted(&mut self) -> Result<(), Arm7tdmiError> {
        if self.is_halted()? {
            return Ok(());
        }
        tracing::debug!("Core unexpectedly not halted, retrying halt once before failing");
        // Unit 1: unit 0 holds the SVC vector catch when semihosting is enabled.
        self.halt_precisely_with_unit(1)?;
        if self.is_halted()? {
            return Ok(());
        }
        Err(Arm7tdmiError::CoreNotHalted)
    }

    /// Durably latch a halt caused by a watchpoint match: after a match, DBGACK alone does not
    /// stop the core on this silicon, and writing `STICKY_HALT | INTDIS` directly does not
    /// either; only the same DBGRQ-then-`STICKY_HALT` transition as [`Self::halt`] does.
    ///
    /// Call once when a poll loop first sees the halt, not from [`Self::is_halted`]: asserting
    /// DBGRQ during its precondition checks broke bulk memory writes.
    pub(crate) fn latch_watchpoint_halt(&mut self) -> Result<(), Arm7tdmiError> {
        // Read TBIT before latching (see `enter_debug_state`).
        let status = self.read_debug_status()?;
        self.latch_current_halt()?;
        // `dbgrq: false`: the DBGRQ pulse only latches a halt the match already caused at a
        // fetch boundary (OpenOCD distinguishes by `debug_reason`, not the DBGRQ bit).
        self.enter_debug_state(status, false)?;
        Ok(())
    }

    /// The DBGRQ-then-`STICKY_HALT` latch of [`Self::latch_watchpoint_halt`], without
    /// `enter_debug_state` (PC capture and Thumb conversion), for `Arm7tdmi::step`, which knows
    /// where the core is. Needed after [`Self::is_halted_with_syscomp`] too, which is not
    /// durable either.
    pub(crate) fn latch_current_halt(&mut self) -> Result<(), Arm7tdmiError> {
        self.write_debug_control(debug_control::DBGRQ)?;
        self.write_debug_control(debug_control::STICKY_HALT | debug_control::INTDIS)?;
        Ok(())
    }

    /// Whether the core is halted in Thumb state (Debug Status `TBIT`). A chain-2 status read,
    /// so it doesn't disturb the core.
    pub(crate) fn halted_in_thumb(&mut self) -> Result<bool, Arm7tdmiError> {
        Ok(self.read_debug_status()? & debug_status::TBIT != 0)
    }

    /// Like [`Self::is_halted`], but also requires `SYSCOMP`, like OpenOCD's
    /// `arm7_9_execute_sys_speed` as used by `arm7_9_step`: DBGACK alone can assert before the
    /// core reached the step watchpoint.
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
        // 10ms reset pulse, then 200ms for the boot ROM before any JTAG activity (OpenOCD's
        // config for this board, `axm0432_jtag`, uses a 200ms `jtag_ntrst_delay`).
        self.probe.target_reset_assert()?;
        std::thread::sleep(Duration::from_millis(10));
        self.probe.target_reset_deassert()?;
        std::thread::sleep(Duration::from_millis(200));

        // The core no longer sits at the halt the resume bookkeeping describes.
        self.state.in_debug_state = false;
        self.state.pending_resume_pc = None;
        self.state.pending_resume_thumb = false;
        self.state.debug_entry_done = false;
        self.state.halt_cpsr = None;
        self.state.halt_requested = false;

        self.init()?;
        self.clear_halt_record()
    }

    /// The number of hardware breakpoint/watchpoint units the EmbeddedICE module provides.
    pub const HW_BREAKPOINT_UNIT_COUNT: usize = 2;

    /// Configure hardware breakpoint/watchpoint unit `index` to halt the core when it fetches
    /// `address` (an instruction-fetch breakpoint, address bit 0 masked - see below).
    ///
    /// Values from ARM DDI 0029G Appendix B and OpenOCD's `embeddedice.h`/`arm7_9_common.c`:
    /// data bus masked out, only `nOPC` compared, so it triggers on instruction fetch.
    ///
    /// The address mask is `1` (bit 0 don't-care), like OpenOCD's `arm7_9_set_breakpoint`. An
    /// exact match misses an even Thumb address right after an ARM-to-Thumb `BX`. Real fetch
    /// addresses always have bit 0 clear, so this never aliases two instructions.
    pub fn set_hw_breakpoint(&mut self, index: usize, address: u32) -> Result<(), Arm7tdmiError> {
        self.configure_fetch_watchpoint(index, address, 1)?;
        tracing::debug!(
            "Set ARM7TDMI hardware breakpoint #{index} at address {:#010x}",
            address
        );
        Ok(())
    }

    /// Halt a running core at the next instruction that reaches execute, by borrowing hardware
    /// unit `index` for a wildcard instruction-fetch watchpoint instead of using DBGRQ. The
    /// unit's registers are saved first and written back afterwards, so a breakpoint or
    /// watchpoint on it is only inactive while the halt takes effect. Returns whether the core
    /// halted.
    ///
    /// A DBGRQ halt stops the core wherever its pipeline is, and its fixed PC correction (as in
    /// OpenOCD) is only right when the pipeline was filled sequentially: during the refill
    /// after a Thumb `BX` it reports a PC one instruction early (verified on the MC1322x,
    /// resuming there derails the target). A watchpoint only fires for an instruction that
    /// reaches execute, so its halt PC is exact.
    pub(crate) fn halt_at_next_instruction(&mut self, index: usize) -> Result<bool, Arm7tdmiError> {
        if self.is_halted()? {
            self.latch_watchpoint_halt()?;
            return Ok(true);
        }
        let registers = EmbeddedIceRegister::unit_registers(index)
            .ok_or(Arm7tdmiError::InvalidBreakpointUnit(index))?;
        let mut saved = [0u32; 6];
        for (i, (value, register)) in saved.iter_mut().zip(registers).enumerate() {
            if i != UNIT_DATA_VALUE {
                *value = self.read_ice_register(register)?;
            }
        }

        // Restore the unit on every path, also after an error: a wildcard watchpoint left
        // armed would halt the core again right after every later resume.
        let halted: Result<bool, Arm7tdmiError> = (|| {
            self.set_wildcard_breakpoint(index)?;
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(100) {
                if self.is_halted()? {
                    self.state.halt_requested = true;
                    self.latch_watchpoint_halt()?;
                    return Ok(true);
                }
            }
            Ok(false)
        })();
        let restored = self.restore_unit(registers, &saved);
        let halted = halted?;
        restored?;
        Ok(halted)
    }

    /// Write back a hardware unit's registers saved by [`Self::halt_at_next_instruction`]. The
    /// control value goes last, so the unit only becomes active again once it is fully restored.
    /// The data value is left alone: no unit configuration uses it, and it holds the halt record
    /// (see [`Self::record_halt_state`]) that the halt just wrote.
    fn restore_unit(
        &mut self,
        registers: [EmbeddedIceRegister; 6],
        saved: &[u32; 6],
    ) -> Result<(), Arm7tdmiError> {
        const CONTROL_VALUE: usize = 4;
        self.write_ice_register(registers[CONTROL_VALUE], 0)?;
        for (i, (&value, register)) in saved.iter().zip(registers).enumerate() {
            if i != CONTROL_VALUE && i != UNIT_DATA_VALUE {
                self.write_ice_register(register, value)?;
            }
        }
        self.write_ice_register(registers[CONTROL_VALUE], saved[CONTROL_VALUE])
    }

    /// Halt the core with an exact halt PC, borrowing `index` for it (see
    /// [`Self::halt_at_next_instruction`]), falling back to a DBGRQ halt if the watchpoint
    /// doesn't fire.
    pub(crate) fn halt_precisely_with_unit(&mut self, index: usize) -> Result<(), Arm7tdmiError> {
        if !self.halt_at_next_instruction(index)? {
            self.halt()?;
        }
        Ok(())
    }

    /// Configure unit `index` to match every instruction fetch (address mask all-ones). Used by
    /// `halt_at_next_instruction`; not usable for single-stepping (see the `step_sim` module).
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

    /// Configure unit `index` to halt on a data access (read or write) to `address`, unlike
    /// [`Self::set_hw_breakpoint`]'s fetch trigger: exact address match, `N_OPC` = 1 (data
    /// access) as the only compared bit.
    pub fn set_hw_data_watchpoint(
        &mut self,
        index: usize,
        address: u32,
    ) -> Result<(), Arm7tdmiError> {
        let Some((addr_value, addr_mask, data_mask, control_value)) =
            EmbeddedIceRegister::watchpoint_unit(index)
        else {
            return Err(Arm7tdmiError::InvalidBreakpointUnit(index));
        };
        let control_mask = EmbeddedIceRegister::control_mask_register(index).unwrap();

        self.write_ice_register(addr_value, address)?;
        self.write_ice_register(addr_mask, 0)?;
        self.write_ice_register(data_mask, 0xFFFF_FFFF)?;
        self.write_ice_register(
            control_value,
            watchpoint_control::ENABLE | watchpoint_control::N_OPC,
        )?;
        self.write_ice_register(control_mask, !watchpoint_control::N_OPC & 0xFF)?;
        tracing::debug!("Set ARM7TDMI data watchpoint #{index} at address {address:#010x}");
        Ok(())
    }

    /// Configure both units as a range-chained single-step trigger, like OpenOCD's
    /// `arm7_9_enable_eice_step`.
    ///
    /// A lone watchpoint one instruction ahead does not reliably fire: the comparator only
    /// works once the core has passed the RESTART/pipeline-flush transient.
    ///
    /// Unit 1 exactly matches `current_pc` with `ENABLE` clear; its range output feeds unit 0,
    /// a wildcard fetch match that also compares `RANGE`. So unit 0 fires on the first fetch
    /// after the current instruction's. If `next_pc == current_pc` (branch to
    /// self) there is no later fetch to gate, so, like OpenOCD, unit 0 is disabled and unit 1
    /// is a plain exact breakpoint.
    ///
    /// Also sets `pending_resume_pc` to `current_pc`, so the following `resume()` doesn't do a
    /// chain-1 PC capture between these chain-2 writes and its RESTART.
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
