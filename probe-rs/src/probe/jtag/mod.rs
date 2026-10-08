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
pub mod dap;
pub use chain::JtagChain;

use bitvec::{slice::BitSlice, vec::BitVec};

use super::{
    Batch, BatchExecutionError, BitSequence, CommandResult, DebugProbe, DebugProbeError, Handle,
    Results, queue::HandleId,
};
use probe_rs_target::ScanChainElement;

pub(crate) use self::chain::ChainParams;

/// Scan chain state held by a JTAG probe between batch runs.
#[derive(Debug)]
pub struct JtagChainState {
    /// The stable state that the TAP rests in between two batches.
    pub tap_state: TapState,

    /// The expected scan chain.
    pub expected_scan_chain: Option<Vec<ScanChainElement>>,

    /// The actual scan chain.
    pub scan_chain: Vec<ScanChainElement>,

    /// The IDCODEs that the last scan found, with one entry for each element of `scan_chain`.
    /// `None` is a TAP without an IDCODE register. Empty when `scan_chain` was not measured.
    pub idcodes: Vec<Option<u32>>,

    /// The parameters of the scan chain.
    pub chain_params: ChainParams,
}

impl Default for JtagChainState {
    fn default() -> Self {
        Self {
            tap_state: TapState::TestLogicReset,
            expected_scan_chain: None,
            scan_chain: Vec::new(),
            idcodes: Vec::new(),
            chain_params: ChainParams::default(),
        }
    }
}

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

/// Any of the sixteen states of the TAP controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FullTapState {
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

impl FullTapState {
    /// The state after one TCK clock with `tms`.
    pub(crate) fn step(self, tms: bool) -> Self {
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

    /// The stable state, if the TAP is in one.
    pub(crate) fn stable(self) -> Option<TapState> {
        match self {
            Self::TestLogicReset => Some(TapState::TestLogicReset),
            Self::RunTestIdle => Some(TapState::RunTestIdle),
            Self::ShiftIr => Some(TapState::ShiftIr),
            Self::ShiftDr => Some(TapState::ShiftDr),
            Self::PauseIr => Some(TapState::PauseIr),
            Self::PauseDr => Some(TapState::PauseDr),
            _ => None,
        }
    }
}

impl From<TapState> for FullTapState {
    fn from(state: TapState) -> Self {
        match state {
            TapState::TestLogicReset => Self::TestLogicReset,
            TapState::RunTestIdle => Self::RunTestIdle,
            TapState::ShiftIr => Self::ShiftIr,
            TapState::ShiftDr => Self::ShiftDr,
            TapState::PauseIr => Self::PauseIr,
            TapState::PauseDr => Self::PauseDr,
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
    /// concatenate, also across batches. The preceding [`JtagOp::EnterState`],
    /// or the state that the previous batch left, selects the register. This
    /// operation does not select the register.
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

/// A probe that runs JTAG batches and holds the state of its scan chain.
pub trait JtagChainAccess: JtagProbe {
    /// Returns a mutable reference to the driver state.
    fn chain_state(&mut self) -> &mut JtagChainState;

    /// Returns the driver state.
    fn chain_state_ref(&self) -> &JtagChainState;
}

/// Bit-banging JTAG interface for probe drivers.
///
/// A blanket [`JtagProbe`] implementation lowers each batch to single bits.
///
/// - [`BitbangJtag::shift`] does not track the TAP state. The lowering tracks it.
/// - A driver that buffers bits sends them at [`BitbangJtag::flush`]. The lowering
///   calls flush before it reads with [`BitbangJtag::captured`].
/// - A TAP reset is [`JtagOp::EnterState`] with [`TapState::TestLogicReset`].
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

/// One step of a JTAG batch on the wire.
pub(crate) enum Step<'a> {
    /// Clock `path` on TMS, with TDI at `tdi`.
    Tms { path: &'a [bool], tdi: bool },
    /// Shift `data` on TDI with TMS low. With `exit`, the last bit is clocked with TMS high
    /// instead, which is the first step out of Shift. `exit` is only set when `data` is not
    /// empty.
    Shift {
        data: &'a BitSequence,
        exit: bool,
        capture: bool,
    },
    /// Clock `count` idle cycles with TMS at `tms`, which holds the TAP in its stable state.
    Clock { count: u32, tms: bool },
}

/// Walk `batch` from `state`, and hand each step to `emit`.
///
/// An exchange with data that the batch follows with a move out of Shift takes the first step
/// of that move with its last bit, so the move leaves that step out. `state` is the last state that a
/// move reached, also when `emit` fails.
pub(crate) fn walk_batch(
    state: &mut TapState,
    batch: &JtagBatch,
    mut emit: impl FnMut(Step<'_>) -> Result<(), DebugProbeError>,
) -> Result<(), DebugProbeError> {
    let ops: Vec<_> = batch.iter().collect();
    let mut skip = 0;
    for (index, (id, op)) in ops.iter().enumerate() {
        match op {
            JtagOp::EnterState(target) => {
                emit(Step::Tms {
                    path: &state.path_to(*target)[skip..],
                    tdi: enter_tdi(*target),
                })?;
                skip = 0;
                *state = *target;
            }
            JtagOp::Exchange { data, capture } => {
                if !matches!(*state, TapState::ShiftIr | TapState::ShiftDr) {
                    return Err(DebugProbeError::Other(format!(
                        "Exchange in state {state:?}, but ShiftIr or ShiftDr is required"
                    )));
                }
                let exit = !data.is_empty()
                    && exchange_leaves_shift(*state, ops.get(index + 1).map(|(_, op)| op));
                emit(Step::Shift {
                    data,
                    exit,
                    capture: *capture && id.should_capture(),
                })?;
                skip = usize::from(exit);
            }
            JtagOp::ClockTck { count } => emit(Step::Clock {
                count: *count,
                tms: *state == TapState::TestLogicReset,
            })?,
        }
    }
    Ok(())
}

pub(crate) fn captured_bits_to_bytes(bits: impl IntoIterator<Item = bool>) -> Vec<u8> {
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

/// Group consecutive TMS bits that share the same value.
pub(crate) fn tms_runs(path: &[bool]) -> Vec<(bool, usize)> {
    let mut runs = Vec::new();
    let mut index = 0;
    while index < path.len() {
        let value = path[index];
        let mut length = 1;
        while index + length < path.len() && path[index + length] == value {
            length += 1;
        }
        runs.push((value, length));
        index += length;
    }
    runs
}

/// Split the bits that a batch captured over the handles that asked for them.
///
/// The probe returns one run of bits for the whole batch, in the order of the
/// operations.
pub(crate) fn distribute_captures<'a>(
    ops: impl IntoIterator<Item = &'a (HandleId, JtagOp)>,
    captured: &BitSlice,
    mut results: Results,
) -> Result<Results, BatchExecutionError<DebugProbeError>> {
    let mut offset = 0usize;
    for (id, op) in ops {
        let JtagOp::Exchange {
            data,
            capture: true,
        } = op
        else {
            continue;
        };
        if !id.should_capture() {
            continue;
        }

        let len = data.len();
        let Some(bits) = captured.get(offset..offset + len) else {
            return Err(BatchExecutionError::new_from_debug_probe(
                DebugProbeError::Other(format!(
                    "The probe captured {} bits, but the batch needs {}",
                    captured.len(),
                    offset + len
                )),
                results,
            ));
        };
        offset += len;
        results.push(
            id,
            CommandResult::VecU8(captured_bits_to_bytes(bits.iter().map(|b| *b))),
        );
    }

    Ok(results)
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
    let results = Results::new();
    let walked = walk_batch(state, batch, |step| match step {
        Step::Tms { path, tdi } => path
            .iter()
            .try_for_each(|&tms| probe.shift(tms, tdi, false)),
        Step::Shift {
            data,
            exit,
            capture,
        } => {
            let last = data.len().saturating_sub(1);
            (0..data.len())
                .try_for_each(|index| probe.shift(exit && index == last, data[index], capture))
        }
        Step::Clock { count, tms } => (0..count).try_for_each(|_| probe.shift(tms, false, false)),
    });
    if let Err(error) = walked {
        return Err(BatchExecutionError::new_from_debug_probe(error, results));
    }

    if let Err(error) = probe.flush() {
        return Err(BatchExecutionError::new_from_debug_probe(error, results));
    }
    let captured = match probe.captured() {
        Ok(bits) => bits,
        Err(error) => return Err(BatchExecutionError::new_from_debug_probe(error, results)),
    };

    distribute_captures(batch.iter(), &captured, results)
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

#[cfg(test)]
pub(crate) mod golden;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{DebugProbe, DebugProbeError, JtagChainState, WireProtocol};

    const STABLE_STATES: [TapState; 6] = [
        TapState::TestLogicReset,
        TapState::RunTestIdle,
        TapState::ShiftIr,
        TapState::ShiftDr,
        TapState::PauseIr,
        TapState::PauseDr,
    ];

    impl FullTapState {
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

    fn walk_path(from: TapState, to: TapState) -> (FullTapState, Vec<FullTapState>) {
        let path = from.path_to(to);
        let mut state = FullTapState::from(from);
        let mut visited = vec![state];
        for &tms in path {
            state = state.step(tms);
            visited.push(state);
        }
        (state, visited)
    }

    #[test]
    fn an_exchange_of_any_length_ends_where_the_batch_says() {
        for bits in [0, 1, 5] {
            let mut batch = JtagBatch::new();
            batch.enter(TapState::ShiftDr);
            batch.exchange_no_capture(BitSequence::repeat(false, bits));
            batch.enter(TapState::RunTestIdle);
            let end = golden::lowering_batch(TapState::RunTestIdle, &batch)
                .into_iter()
                .fold(FullTapState::RunTestIdle, |state, (tms, _, _)| {
                    state.step(tms)
                });
            assert_eq!(end, FullTapState::RunTestIdle, "{bits} bits");
        }
    }

    #[test]
    fn clocks_hold_every_stable_state() {
        for state in STABLE_STATES {
            let mut batch = JtagBatch::new();
            batch.clock(3);
            let end = golden::lowering_batch(state, &batch)
                .into_iter()
                .fold(FullTapState::from(state), |state, (tms, _, _)| {
                    state.step(tms)
                });
            assert_eq!(end, FullTapState::from(state), "{state:?}");
        }
    }

    #[test]
    fn path_to_reaches_target_for_all_pairs() {
        for from in STABLE_STATES {
            for to in STABLE_STATES {
                let (end, _) = walk_path(from, to);
                assert_eq!(end, FullTapState::from(to), "from {:?} to {:?}", from, to);
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
                let mut state = FullTapState::from(from);
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
    fn icepick_zero_bit_scan_uses_enter_state() {
        #[derive(Debug)]
        struct ShiftRecorder {
            triples: Vec<(bool, bool)>,
            jtag_state: JtagChainState,
        }

        impl ShiftRecorder {
            fn new() -> Self {
                Self {
                    triples: Vec::new(),
                    jtag_state: JtagChainState::default(),
                }
            }
        }

        impl DebugProbe for ShiftRecorder {
            fn get_name(&self) -> &str {
                "shift recorder"
            }

            fn speed_khz(&self) -> u32 {
                0
            }

            fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
                Ok(speed_khz)
            }

            fn attach(&mut self) -> Result<(), DebugProbeError> {
                Ok(())
            }

            fn detach(&mut self) -> Result<(), crate::Error> {
                Ok(())
            }

            fn target_reset(&mut self) -> Result<(), DebugProbeError> {
                Err(DebugProbeError::CommandNotSupportedByProbe {
                    command_name: "target_reset",
                })
            }

            fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
                Err(DebugProbeError::CommandNotSupportedByProbe {
                    command_name: "target_reset_assert",
                })
            }

            fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
                Ok(())
            }

            fn select_protocol(&mut self, _protocol: WireProtocol) -> Result<(), DebugProbeError> {
                Ok(())
            }

            fn active_protocol(&self) -> Option<WireProtocol> {
                None
            }

            fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
                self
            }
        }

        impl BitbangJtag for ShiftRecorder {
            fn tap_state(&mut self) -> &mut TapState {
                &mut self.jtag_state.tap_state
            }

            fn shift(
                &mut self,
                tms: bool,
                tdi: bool,
                _capture: bool,
            ) -> Result<(), DebugProbeError> {
                self.triples.push((tms, tdi));
                Ok(())
            }

            fn flush(&mut self) -> Result<(), DebugProbeError> {
                Ok(())
            }

            fn captured(&mut self) -> Result<BitVec, DebugProbeError> {
                Ok(BitVec::new())
            }
        }

        impl JtagChainAccess for ShiftRecorder {
            fn chain_state(&mut self) -> &mut JtagChainState {
                &mut self.jtag_state
            }

            fn chain_state_ref(&self) -> &JtagChainState {
                &self.jtag_state
            }
        }

        use super::chain::JtagChain;

        let mut probe = ShiftRecorder::new();
        probe.jtag_state.tap_state = TapState::RunTestIdle;
        let mut chain = JtagChain::new(&mut probe);
        let mut batch = JtagBatch::new();
        batch.enter(TapState::PauseDr);
        chain.run(batch).unwrap();
        let mut batch = JtagBatch::new();
        batch.enter(TapState::RunTestIdle);
        chain.run(batch).unwrap();

        let tms = probe
            .triples
            .iter()
            .map(|(t, _)| if *t { '1' } else { '0' })
            .collect::<String>();
        let tdi = probe
            .triples
            .iter()
            .map(|(_, d)| if *d { '1' } else { '0' })
            .collect::<String>();

        assert_eq!(tms, "1010110");
        assert_eq!(tdi, "0000000");
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
