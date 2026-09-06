//! Layer 0 JTAG operations for probe drivers.
//!
//! The scan-chain layer above consumes these operations.
//!
//! No operation carries a TMS bit and a TDI bit at the same time. A lowering
//! may merge the last bit of a [`JtagOp::Exchange`] into the following
//! [`JtagOp::EnterState`].
//!
//! [`TapState::path_to`] is the source of the path table.
//!
//! # Example
//!
//! ```
//! use probe_rs::probe::{BitSequence, JtagBatch, TapState};
//!
//! let mut batch = JtagBatch::new();
//! batch.enter(TapState::ShiftIr);
//! batch.exchange(BitSequence::from_u64(32, 0x1234_5678));
//! batch.enter(TapState::ShiftDr);
//! batch.exchange(BitSequence::from_u64(32, 0xDEAD_BEEF));
//! batch.enter(TapState::RunTestIdle);
//! batch.clock(8);
//! ```
pub mod chain;
pub use chain::JtagChain;

use bitvec::vec::BitVec;

use super::{
    Batch, BatchExecutionError, BitSequence, CommandResult, DebugProbe, DebugProbeError, Handle,
    RawJtagIo, Results,
};

/// A stable TAP state that a caller may target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapState {
    /// Test-Logic-Reset.
    TestLogicReset,
    /// Run-Test/Idle.
    RunTestIdle,
    /// Shift-IR.
    ShiftIr,
    /// Shift-DR.
    ShiftDr,
    /// Pause-IR.
    PauseIr,
    /// Pause-DR.
    PauseDr,
}

const TLR_TO_TLR: [bool; 5] = [true; 5];
const TLR_TO_RTI: [bool; 1] = [false];
const TLR_TO_SHIFT_IR: [bool; 5] = [false, true, true, false, false];
const TLR_TO_SHIFT_DR: [bool; 4] = [false, true, false, false];
const TLR_TO_PAUSE_IR: [bool; 6] = [false, true, true, false, true, false];
const TLR_TO_PAUSE_DR: [bool; 5] = [false, true, false, true, false];

const RTI_TO_SHIFT_IR: [bool; 4] = [true, true, false, false];
const RTI_TO_SHIFT_DR: [bool; 3] = [true, false, false];
const RTI_TO_PAUSE_IR: [bool; 5] = [true, true, false, true, false];
const RTI_TO_PAUSE_DR: [bool; 4] = [true, false, true, false];

const SHIFT_IR_TO_RTI: [bool; 3] = [true, true, false];
const SHIFT_IR_TO_SHIFT_DR: [bool; 5] = [true, true, true, false, false];
const SHIFT_IR_TO_PAUSE_IR: [bool; 2] = [true, false];
const SHIFT_IR_TO_PAUSE_DR: [bool; 6] = [true, true, true, false, true, false];

const SHIFT_DR_TO_RTI: [bool; 3] = [true, true, false];
const SHIFT_DR_TO_SHIFT_IR: [bool; 6] = [true, true, true, true, false, false];
const SHIFT_DR_TO_PAUSE_IR: [bool; 7] = [true, true, true, true, false, true, false];
const SHIFT_DR_TO_PAUSE_DR: [bool; 2] = [true, false];

const PAUSE_IR_TO_RTI: [bool; 3] = [true, true, false];
const PAUSE_IR_TO_SHIFT_IR: [bool; 2] = [true, false];
const PAUSE_IR_TO_SHIFT_DR: [bool; 5] = [true, true, true, false, false];
const PAUSE_IR_TO_PAUSE_DR: [bool; 6] = [true, true, true, false, true, false];

const PAUSE_DR_TO_RTI: [bool; 3] = [true, true, false];
const PAUSE_DR_TO_SHIFT_IR: [bool; 6] = [true, true, true, true, false, false];
const PAUSE_DR_TO_SHIFT_DR: [bool; 2] = [true, false];
const PAUSE_DR_TO_PAUSE_IR: [bool; 7] = [true, true, true, true, false, true, false];

impl TapState {
    /// Returns the TMS bits that move the TAP from `self` to `target`.
    pub fn path_to(self, target: TapState) -> &'static [bool] {
        match (self, target) {
            (TapState::TestLogicReset, TapState::TestLogicReset) => &TLR_TO_TLR,
            (TapState::TestLogicReset, TapState::RunTestIdle) => &TLR_TO_RTI,
            (TapState::TestLogicReset, TapState::ShiftIr) => &TLR_TO_SHIFT_IR,
            (TapState::TestLogicReset, TapState::ShiftDr) => &TLR_TO_SHIFT_DR,
            (TapState::TestLogicReset, TapState::PauseIr) => &TLR_TO_PAUSE_IR,
            (TapState::TestLogicReset, TapState::PauseDr) => &TLR_TO_PAUSE_DR,

            (TapState::RunTestIdle, TapState::TestLogicReset) => &TLR_TO_TLR,
            (TapState::RunTestIdle, TapState::RunTestIdle) => &[],
            (TapState::RunTestIdle, TapState::ShiftIr) => &RTI_TO_SHIFT_IR,
            (TapState::RunTestIdle, TapState::ShiftDr) => &RTI_TO_SHIFT_DR,
            (TapState::RunTestIdle, TapState::PauseIr) => &RTI_TO_PAUSE_IR,
            (TapState::RunTestIdle, TapState::PauseDr) => &RTI_TO_PAUSE_DR,

            (TapState::ShiftIr, TapState::TestLogicReset) => &TLR_TO_TLR,
            (TapState::ShiftIr, TapState::RunTestIdle) => &SHIFT_IR_TO_RTI,
            (TapState::ShiftIr, TapState::ShiftIr) => &[],
            (TapState::ShiftIr, TapState::ShiftDr) => &SHIFT_IR_TO_SHIFT_DR,
            (TapState::ShiftIr, TapState::PauseIr) => &SHIFT_IR_TO_PAUSE_IR,
            (TapState::ShiftIr, TapState::PauseDr) => &SHIFT_IR_TO_PAUSE_DR,

            (TapState::ShiftDr, TapState::TestLogicReset) => &TLR_TO_TLR,
            (TapState::ShiftDr, TapState::RunTestIdle) => &SHIFT_DR_TO_RTI,
            (TapState::ShiftDr, TapState::ShiftIr) => &SHIFT_DR_TO_SHIFT_IR,
            (TapState::ShiftDr, TapState::ShiftDr) => &[],
            (TapState::ShiftDr, TapState::PauseIr) => &SHIFT_DR_TO_PAUSE_IR,
            (TapState::ShiftDr, TapState::PauseDr) => &SHIFT_DR_TO_PAUSE_DR,

            (TapState::PauseIr, TapState::TestLogicReset) => &TLR_TO_TLR,
            (TapState::PauseIr, TapState::RunTestIdle) => &PAUSE_IR_TO_RTI,
            (TapState::PauseIr, TapState::ShiftIr) => &PAUSE_IR_TO_SHIFT_IR,
            (TapState::PauseIr, TapState::ShiftDr) => &PAUSE_IR_TO_SHIFT_DR,
            (TapState::PauseIr, TapState::PauseIr) => &[],
            (TapState::PauseIr, TapState::PauseDr) => &PAUSE_IR_TO_PAUSE_DR,

            (TapState::PauseDr, TapState::TestLogicReset) => &TLR_TO_TLR,
            (TapState::PauseDr, TapState::RunTestIdle) => &PAUSE_DR_TO_RTI,
            (TapState::PauseDr, TapState::ShiftIr) => &PAUSE_DR_TO_SHIFT_IR,
            (TapState::PauseDr, TapState::ShiftDr) => &PAUSE_DR_TO_SHIFT_DR,
            (TapState::PauseDr, TapState::PauseIr) => &PAUSE_DR_TO_PAUSE_IR,
            (TapState::PauseDr, TapState::PauseDr) => &[],
        }
    }
}

/// One JTAG operation in a batch.
#[derive(Clone, Debug)]
pub enum JtagOp {
    /// Move the TAP to a stable state.
    EnterState(TapState),

    /// Shift bits through the selected register.
    ///
    /// The TAP stays in `Shift-Ir` or `Shift-Dr`, so two neighbour exchanges
    /// concatenate. The preceding [`JtagOp::EnterState`] selects the
    /// register. This operation does not select the register.
    Exchange {
        /// TDI bits to shift.
        data: BitSequence,
        /// Whether to capture TDO.
        capture: bool,
    },

    /// Clock TCK and hold the current stable state.
    ///
    /// Legal in a stable state only. The TAP stays in that state.
    ClockTck {
        /// Number of TCK cycles.
        count: u32,
    },
}

/// A batch of JTAG operations.
pub type JtagBatch = Batch<JtagOp, DebugProbeError>;

impl JtagBatch {
    /// Schedule a state transition.
    pub fn enter(&mut self, state: TapState) {
        let _ = self.schedule(JtagOp::EnterState(state));
    }

    /// Schedule an exchange and return a handle for the captured TDO bits.
    pub fn exchange(&mut self, data: BitSequence) -> Handle<BitSequence> {
        let bit_len = data.len();
        self.schedule(JtagOp::Exchange {
            data,
            capture: true,
        })
        .map(move |result| match result {
            CommandResult::VecU8(bytes) => BitSequence::from_bytes(&bytes, bit_len),
            _ => panic!("unexpected CommandResult variant for a JTAG exchange"),
        })
    }

    /// Schedule an exchange without capturing TDO.
    pub fn exchange_no_capture(&mut self, data: BitSequence) {
        let _ = self.schedule(JtagOp::Exchange {
            data,
            capture: false,
        });
    }

    /// Schedule idle TCK clocks in the current stable state.
    pub fn clock(&mut self, count: u32) {
        let _ = self.schedule(JtagOp::ClockTck { count });
    }
}

/// A probe that executes [`JtagBatch`] values.
///
/// The implementation must follow the path table of [`TapState::path_to`]. The
/// batch states the intent. The implementation must not infer intent from the
/// bits.
///
/// The implementation may reorder nothing.
///
/// The implementation may skip the capture of a [`JtagOp::Exchange`] whose
/// handle the caller dropped. `HandleId::should_capture` reports this.
///
/// The implementation may merge the last bit of a [`JtagOp::Exchange`] into
/// the [`JtagOp::EnterState`] that follows it.
///
/// On a fault, the implementation returns the results before the fault, and the
/// index of the operation that failed.
pub trait JtagProbe: DebugProbe {
    /// Execute a batch of JTAG operations.
    fn run_batch(
        &mut self,
        batch: &JtagBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>>;

    /// Program IR lengths into the probe firmware, if it stores them.
    ///
    /// CMSIS-DAP `DAP_JTAG_Configure` is the one caller. The default body
    /// returns `Ok(())`. This is not a wire transfer. Do not add a [`JtagOp`]
    /// for it.
    fn configure_jtag(&mut self, _skip_scan: bool) -> Result<(), DebugProbeError> {
        Ok(())
    }
}

/// Bit-banging JTAG interface for probe drivers.
///
/// Three differences from [`RawJtagIo`]:
///
/// - [`BitbangJtag::shift`] does not track the TAP state. The lowering tracks it.
/// - [`BitbangJtag::flush`] is new. A driver that buffers bits sends them at a
///   flush. The lowering calls flush before it reads with [`BitbangJtag::captured`].
///   Today [`RawJtagIo::read_captured_bits`] does both jobs.
/// - This trait has no `reset_jtag_state_machine`. [`JtagOp::EnterState`] with
///   [`TapState::TestLogicReset`] replaces it.
pub trait BitbangJtag: DebugProbe {
    /// Return the state that the TAP rests in between two batches.
    ///
    /// The lowering reads this state at the start of a batch, and it writes
    /// the new state at the end. A driver only stores the value.
    fn tap_state(&mut self) -> &mut TapState;

    /// Shift one bit through the TAP.
    fn shift(&mut self, tms: bool, tdi: bool, capture: bool) -> Result<(), DebugProbeError>;

    /// Send buffered bits to the probe hardware.
    fn flush(&mut self) -> Result<(), DebugProbeError>;

    /// Return captured TDO bits and clear the capture buffer.
    fn captured(&mut self) -> Result<BitVec, DebugProbeError>;

    /// Shift bits through the TAP.
    fn shift_all(
        &mut self,
        tms: impl IntoIterator<Item = bool>,
        tdi: impl IntoIterator<Item = bool>,
        cap: impl IntoIterator<Item = bool>,
    ) -> Result<(), DebugProbeError> {
        for ((tms, tdi), cap) in tms.into_iter().zip(tdi).zip(cap) {
            self.shift(tms, tdi, cap)?;
        }

        Ok(())
    }
}

fn exchange_leaves_shift(current: TapState, next: Option<&JtagOp>) -> bool {
    match next {
        Some(JtagOp::EnterState(target)) => {
            let path = current.path_to(*target);
            !path.is_empty() && path[0]
        }
        _ => false,
    }
}

fn enter_tdi(target: TapState) -> bool {
    target == TapState::TestLogicReset
}

fn captured_bits_to_bytes(bits: impl IntoIterator<Item = bool>) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut byte = 0u8;
    let mut bit_in_byte = 0usize;
    for bit in bits {
        if bit {
            byte |= 1 << bit_in_byte;
        }
        bit_in_byte += 1;
        if bit_in_byte == 8 {
            bytes.push(byte);
            byte = 0;
            bit_in_byte = 0;
        }
    }
    if bit_in_byte != 0 {
        bytes.push(byte);
    }
    bytes
}

/// Lower a batch to bit-banging, starting from `start`.
///
/// The TAP rests where the batch leaves it. The caller writes that state back,
/// so the next batch starts from it.
pub(crate) fn run_bitbang_batch<P: BitbangJtag>(
    probe: &mut P,
    start: TapState,
    batch: &JtagBatch,
) -> Result<Results, BatchExecutionError<DebugProbeError>> {
    let mut state = start;
    let result = lower_batch(probe, &mut state, batch);
    *probe.tap_state() = state;
    result
}

fn lower_batch<P: BitbangJtag>(
    probe: &mut P,
    state: &mut TapState,
    batch: &JtagBatch,
) -> Result<Results, BatchExecutionError<DebugProbeError>> {
    let ops: Vec<_> = batch.iter().collect();
    let mut results = Results::new();
    let mut skip_enter_path_bits = 0usize;

    for (index, (id, op)) in ops.iter().enumerate() {
        match op {
            JtagOp::EnterState(target) => {
                let target = *target;
                let path = &state.path_to(target)[skip_enter_path_bits..];
                skip_enter_path_bits = 0;
                let tdi = enter_tdi(target);
                for &tms in path {
                    if let Err(error) = probe.shift(tms, tdi, false) {
                        return Err(BatchExecutionError::new_from_debug_probe(error, results));
                    }
                }
                *state = target;
            }
            JtagOp::Exchange { data, capture } => {
                if *state != TapState::ShiftIr && *state != TapState::ShiftDr {
                    return Err(BatchExecutionError::new_from_debug_probe(
                        DebugProbeError::Other(format!(
                            "Exchange in state {state:?}, but ShiftIr or ShiftDr is required"
                        )),
                        results,
                    ));
                }
                let merge_exit =
                    exchange_leaves_shift(*state, ops.get(index + 1).map(|(_, op)| op));
                let do_capture = *capture && id.should_capture();
                let bit_count = data.len();
                for bit_index in 0..bit_count {
                    let is_last = bit_index + 1 == bit_count;
                    let tms = if merge_exit && is_last {
                        // The last exchange bit and the first exit bit are one clock on the wire.
                        skip_enter_path_bits = 1;
                        true
                    } else {
                        false
                    };
                    let tdi = data[bit_index];
                    if let Err(error) = probe.shift(tms, tdi, do_capture) {
                        return Err(BatchExecutionError::new_from_debug_probe(error, results));
                    }
                }
            }
            JtagOp::ClockTck { count } => {
                for _ in 0..*count {
                    if let Err(error) = probe.shift(false, false, false) {
                        return Err(BatchExecutionError::new_from_debug_probe(error, results));
                    }
                }
            }
        }
    }

    if let Err(error) = probe.flush() {
        return Err(BatchExecutionError::new_from_debug_probe(error, results));
    }
    let captured = match probe.captured() {
        Ok(bits) => bits,
        Err(error) => return Err(BatchExecutionError::new_from_debug_probe(error, results)),
    };

    let mut capture_offset = 0usize;
    for (id, op) in ops {
        let JtagOp::Exchange {
            data,
            capture: true,
        } = op
        else {
            continue;
        };
        if id.should_capture() {
            let len = data.len();
            let Some(bits) = captured.get(capture_offset..capture_offset + len) else {
                return Err(BatchExecutionError::new_from_debug_probe(
                    DebugProbeError::Other(format!(
                        "The probe captured {} bits, but the batch needs {}",
                        captured.len(),
                        capture_offset + len
                    )),
                    results,
                ));
            };
            capture_offset += len;
            results.push(
                id,
                CommandResult::VecU8(captured_bits_to_bytes(bits.iter().map(|b| *b))),
            );
        }
    }

    Ok(results)
}

impl<P: BitbangJtag> JtagProbe for P {
    fn run_batch(
        &mut self,
        batch: &JtagBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        let start = *self.tap_state();
        run_bitbang_batch(self, start, batch)
    }
}

/// Temporary bridge from [`RawJtagIo`] to [`BitbangJtag`].
///
/// This bridge goes away when every driver implements [`BitbangJtag`] directly.
#[doc(hidden)]
impl<P: RawJtagIo> BitbangJtag for P {
    fn tap_state(&mut self) -> &mut TapState {
        &mut self.state_mut().tap_state
    }

    fn shift(&mut self, tms: bool, tdi: bool, capture: bool) -> Result<(), DebugProbeError> {
        self.shift_bit(tms, tdi, capture)
    }

    fn flush(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn captured(&mut self) -> Result<BitVec, DebugProbeError> {
        self.read_captured_bits()
    }
}

#[cfg(test)]
mod golden;

#[cfg(test)]
mod tests {
    use super::*;

    const STABLE_STATES: [TapState; 6] = [
        TapState::TestLogicReset,
        TapState::RunTestIdle,
        TapState::ShiftIr,
        TapState::ShiftDr,
        TapState::PauseIr,
        TapState::PauseDr,
    ];

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum ModelState {
        TestLogicReset,
        RunTestIdle,
        SelectDr,
        CaptureDr,
        ShiftDr,
        Exit1Dr,
        PauseDr,
        Exit2Dr,
        UpdateDr,
        SelectIr,
        CaptureIr,
        ShiftIr,
        Exit1Ir,
        PauseIr,
        Exit2Ir,
        UpdateIr,
    }

    impl ModelState {
        fn step(self, tms: bool) -> Self {
            if tms {
                match self {
                    Self::TestLogicReset => Self::TestLogicReset,
                    Self::RunTestIdle => Self::SelectDr,
                    Self::SelectDr => Self::SelectIr,
                    Self::CaptureDr | Self::ShiftDr => Self::Exit1Dr,
                    Self::Exit1Dr | Self::Exit2Dr => Self::UpdateDr,
                    Self::PauseDr => Self::Exit2Dr,
                    Self::UpdateDr => Self::SelectDr,
                    Self::SelectIr => Self::TestLogicReset,
                    Self::CaptureIr | Self::ShiftIr => Self::Exit1Ir,
                    Self::Exit1Ir | Self::Exit2Ir => Self::UpdateIr,
                    Self::PauseIr => Self::Exit2Ir,
                    Self::UpdateIr => Self::SelectDr,
                }
            } else {
                match self {
                    Self::TestLogicReset => Self::RunTestIdle,
                    Self::RunTestIdle => Self::RunTestIdle,
                    Self::SelectDr => Self::CaptureDr,
                    Self::CaptureDr | Self::ShiftDr => Self::ShiftDr,
                    Self::Exit1Dr | Self::PauseDr => Self::PauseDr,
                    Self::Exit2Dr => Self::ShiftDr,
                    Self::UpdateDr => Self::RunTestIdle,
                    Self::SelectIr => Self::CaptureIr,
                    Self::CaptureIr | Self::ShiftIr => Self::ShiftIr,
                    Self::Exit1Ir | Self::PauseIr => Self::PauseIr,
                    Self::Exit2Ir => Self::ShiftIr,
                    Self::UpdateIr => Self::RunTestIdle,
                }
            }
        }

        fn is_capture(self) -> bool {
            matches!(self, Self::CaptureDr | Self::CaptureIr)
        }

        fn is_run_test_idle(self) -> bool {
            self == Self::RunTestIdle
        }

        fn is_test_logic_reset(self) -> bool {
            self == Self::TestLogicReset
        }
    }

    fn tap_to_model(tap: TapState) -> ModelState {
        match tap {
            TapState::TestLogicReset => ModelState::TestLogicReset,
            TapState::RunTestIdle => ModelState::RunTestIdle,
            TapState::ShiftIr => ModelState::ShiftIr,
            TapState::ShiftDr => ModelState::ShiftDr,
            TapState::PauseIr => ModelState::PauseIr,
            TapState::PauseDr => ModelState::PauseDr,
        }
    }

    fn walk_path(from: TapState, to: TapState) -> (ModelState, Vec<ModelState>) {
        let path = from.path_to(to);
        let mut state = tap_to_model(from);
        let mut visited = vec![state];
        for &tms in path {
            state = state.step(tms);
            visited.push(state);
        }
        (state, visited)
    }

    #[test]
    fn path_to_reaches_target_for_all_pairs() {
        for from in STABLE_STATES {
            for to in STABLE_STATES {
                let (end, _) = walk_path(from, to);
                assert_eq!(end, tap_to_model(to), "from {:?} to {:?}", from, to);
            }
        }
    }

    #[test]
    fn path_from_non_tlr_never_visits_tlr_unless_target_is_tlr() {
        for from in STABLE_STATES {
            if from == TapState::TestLogicReset {
                continue;
            }
            for to in STABLE_STATES {
                if to == TapState::TestLogicReset {
                    continue;
                }
                let (_, visited) = walk_path(from, to);
                for state in &visited[..visited.len() - 1] {
                    assert!(
                        !state.is_test_logic_reset(),
                        "from {:?} to {:?} visited TLR",
                        from,
                        to
                    );
                }
            }
        }
    }

    #[test]
    fn capture_only_on_way_to_shift_or_pause() {
        for from in STABLE_STATES {
            for to in STABLE_STATES {
                let (_, visited) = walk_path(from, to);
                let target_is_shift_or_pause = matches!(
                    to,
                    TapState::ShiftIr | TapState::ShiftDr | TapState::PauseIr | TapState::PauseDr
                );
                for state in visited {
                    if state.is_capture() {
                        assert!(
                            target_is_shift_or_pause,
                            "capture on path from {:?} to {:?}",
                            from, to
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn no_extra_run_test_idle_clocks() {
        for from in STABLE_STATES {
            for to in STABLE_STATES {
                let path = from.path_to(to);
                let mut state = tap_to_model(from);
                let mut rti_entries = 0;
                for &tms in path {
                    state = state.step(tms);
                    if state.is_run_test_idle() {
                        rti_entries += 1;
                    }
                }
                let expected = match (from, to) {
                    (TapState::TestLogicReset, TapState::TestLogicReset) => 0,
                    (TapState::RunTestIdle, TapState::RunTestIdle) => 0,
                    (TapState::TestLogicReset, TapState::RunTestIdle) => 1,
                    (TapState::TestLogicReset, _) => 1,
                    (_, TapState::RunTestIdle) => 1,
                    _ => 0,
                };
                assert_eq!(
                    rti_entries, expected,
                    "RTI entries from {:?} to {:?}",
                    from, to
                );
            }
        }
    }

    #[test]
    fn identity_paths_are_empty_except_tlr() {
        for state in STABLE_STATES {
            let path = state.path_to(state);
            if state == TapState::TestLogicReset {
                assert_eq!(path, &[true, true, true, true, true]);
            } else {
                assert!(path.is_empty());
            }
        }
    }

    #[test]
    fn shift_ir_to_shift_dr_avoids_run_test_idle() {
        let path = TapState::ShiftIr.path_to(TapState::ShiftDr);
        assert_eq!(path, &[true, true, true, false, false]);
        let (_, visited) = walk_path(TapState::ShiftIr, TapState::ShiftDr);
        assert!(!visited.iter().any(|state| state.is_run_test_idle()));
    }

    #[test]
    fn shift_dr_to_shift_ir_avoids_run_test_idle() {
        let path = TapState::ShiftDr.path_to(TapState::ShiftIr);
        assert_eq!(path, &[true, true, true, true, false, false]);
        let (_, visited) = walk_path(TapState::ShiftDr, TapState::ShiftIr);
        assert!(!visited.iter().any(|state| state.is_run_test_idle()));
    }

    #[test]
    fn icepick_zero_bit_scan_tms_sequence() {
        let path = TapState::RunTestIdle
            .path_to(TapState::PauseDr)
            .iter()
            .chain(TapState::PauseDr.path_to(TapState::RunTestIdle))
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(
            path,
            [true, false, true, false, true, true, false],
            "ICEPICK ZBS TMS sequence"
        );
    }

    #[test]
    fn batch_of_six_operations() {
        let mut batch = JtagBatch::new();
        batch.enter(TapState::ShiftIr);
        let _instruction = batch.exchange(BitSequence::from_u64(32, 0x1234_5678));
        batch.enter(TapState::ShiftDr);
        let _data = batch.exchange(BitSequence::from_u64(32, 0xDEAD_BEEF));
        batch.enter(TapState::RunTestIdle);
        batch.clock(8);
        assert_eq!(batch.len(), 6);
    }
}
