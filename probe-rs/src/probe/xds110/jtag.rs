//! JTAG batches, run with `OCD_SCAN_REQUEST`.
//!
//! The firmware runs IR and DR scans, run-test and stable clocks. It moves the TAP on the
//! quickest path, which is the path of [`TapState::path_to`]. Other moves are path moves.

use bitvec::vec::BitVec;

use super::{MAX_DATA_BLOCK, OCD_SCAN_REQUEST, Xds110};
use crate::probe::{
    BatchExecutionError, DebugProbeError, HandleId, JtagBatch, JtagChainAccess, JtagChainState,
    JtagOp, JtagProbe, Results, TapState, jtag::distribute_captures,
};

const CMD_IR_SCAN: u8 = 1;
const CMD_DR_SCAN: u8 = 2;
const CMD_RUNTEST: u8 = 3;
const CMD_STABLECLOCKS: u8 = 4;

/// TAP states, as the firmware numbers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum XdsState {
    Reset = 1,
    Idle = 2,
    ShiftDr = 3,
    ShiftIr = 4,
    PauseDr = 5,
    PauseIr = 6,
    Exit1Dr = 8,
    Exit1Ir = 9,
    Exit2Dr = 10,
    Exit2Ir = 11,
    SelectDr = 12,
    SelectIr = 13,
    UpdateDr = 14,
    UpdateIr = 15,
    CaptureDr = 16,
    CaptureIr = 17,
}

impl XdsState {
    /// The state after one TCK with `tms`.
    pub(super) fn next(self, tms: bool) -> Self {
        use XdsState::{
            CaptureDr, CaptureIr, Exit1Dr, Exit1Ir, Exit2Dr, Exit2Ir, Idle, PauseDr, PauseIr,
            Reset, SelectDr, SelectIr, ShiftDr, ShiftIr, UpdateDr, UpdateIr,
        };
        match (self, tms) {
            (Reset, false) => Idle,
            (Reset, true) => Reset,
            (Idle, false) => Idle,
            (Idle, true) => SelectDr,
            (SelectDr, false) => CaptureDr,
            (SelectDr, true) => SelectIr,
            (CaptureDr, false) | (ShiftDr, false) | (Exit2Dr, false) => ShiftDr,
            (CaptureDr, true) | (ShiftDr, true) => Exit1Dr,
            (Exit1Dr, false) | (PauseDr, false) => PauseDr,
            (Exit1Dr, true) | (Exit2Dr, true) => UpdateDr,
            (PauseDr, true) => Exit2Dr,
            (UpdateDr, false) | (UpdateIr, false) => Idle,
            (UpdateDr, true) | (UpdateIr, true) => SelectDr,
            (SelectIr, false) => CaptureIr,
            (SelectIr, true) => Reset,
            (CaptureIr, false) | (ShiftIr, false) | (Exit2Ir, false) => ShiftIr,
            (CaptureIr, true) | (ShiftIr, true) => Exit1Ir,
            (Exit1Ir, false) | (PauseIr, false) => PauseIr,
            (Exit1Ir, true) | (Exit2Ir, true) => UpdateIr,
            (PauseIr, true) => Exit2Ir,
        }
    }

    /// The stable state, if this is one.
    pub(super) fn stable(self) -> Option<TapState> {
        Some(match self {
            XdsState::Reset => TapState::TestLogicReset,
            XdsState::Idle => TapState::RunTestIdle,
            XdsState::ShiftDr => TapState::ShiftDr,
            XdsState::ShiftIr => TapState::ShiftIr,
            XdsState::PauseDr => TapState::PauseDr,
            XdsState::PauseIr => TapState::PauseIr,
            _ => return None,
        })
    }
}

impl From<TapState> for XdsState {
    fn from(state: TapState) -> Self {
        match state {
            TapState::TestLogicReset => XdsState::Reset,
            TapState::RunTestIdle => XdsState::Idle,
            TapState::ShiftDr => XdsState::ShiftDr,
            TapState::ShiftIr => XdsState::ShiftIr,
            TapState::PauseDr => XdsState::PauseDr,
            TapState::PauseIr => XdsState::PauseIr,
        }
    }
}

/// One firmware step of a batch.
#[derive(Debug, PartialEq)]
enum Step {
    /// Entries of an `OCD_SCAN_REQUEST`.
    Scan {
        entry: Vec<u8>,
        /// Bits of the scan that the batch captures, as (offset, length).
        captures: Vec<(usize, usize)>,
        /// Length of the scan in bits.
        bits: usize,
    },
    /// A path move, through the given states.
    Path(Vec<u8>),
}

fn run_test(clocks: u32, end: TapState) -> Step {
    let mut entry = vec![CMD_RUNTEST];
    entry.extend_from_slice(&clocks.to_le_bytes());
    entry.push(XdsState::from(end) as u8);
    Step::Scan {
        entry,
        captures: vec![],
        bits: 0,
    }
}

fn move_to(from: TapState, to: TapState) -> Option<Step> {
    if to == TapState::TestLogicReset
        || (from == to && to == TapState::RunTestIdle)
        || from == TapState::RunTestIdle
        || to == TapState::RunTestIdle
    {
        // Run-test goes to Run-Test/Idle, then to the end state, on the quickest paths.
        return Some(run_test(0, to));
    }
    if from == to {
        return None;
    }
    let mut state = XdsState::from(from);
    Some(Step::Path(
        from.path_to(to)
            .iter()
            .map(|&tms| {
                state = state.next(tms);
                state as u8
            })
            .collect(),
    ))
}

/// The number of bits of the exchanges at the start of `ops`.
fn scan_bits(ops: &[(&HandleId, &JtagOp)]) -> usize {
    ops.iter()
        .map_while(|(_, op)| match op {
            JtagOp::Exchange { data, .. } => Some(data.len()),
            _ => None,
        })
        .sum()
}

/// Translates a batch that starts in `start` into firmware steps, and returns the end state.
fn encode_batch(
    start: TapState,
    batch: &JtagBatch,
) -> Result<(TapState, Vec<Step>), DebugProbeError> {
    let ops: Vec<_> = batch.iter().map(|(id, op)| (id, op)).collect();
    let mut state = start;
    let mut steps = Vec::new();
    let mut index = 0;

    while index < ops.len() {
        match ops[index].1 {
            JtagOp::EnterState(target) => {
                // The firmware hangs on a scan of no bits.
                let starts_scan = matches!(target, TapState::ShiftIr | TapState::ShiftDr)
                    && scan_bits(&ops[index + 1..]) > 0;
                if !starts_scan {
                    steps.extend(move_to(state, *target));
                }
                state = *target;
                index += 1;
            }
            JtagOp::Exchange { .. } => {
                let command = match state {
                    TapState::ShiftIr => CMD_IR_SCAN,
                    TapState::ShiftDr => CMD_DR_SCAN,
                    _ => {
                        return Err(DebugProbeError::Other(format!(
                            "Exchange in state {state:?}, but ShiftIr or ShiftDr is required"
                        )));
                    }
                };
                let mut data = BitVec::<u8>::new();
                let mut captures = Vec::new();
                while let Some((
                    id,
                    JtagOp::Exchange {
                        data: bits,
                        capture,
                    },
                )) = ops.get(index)
                {
                    if *capture && id.should_capture() {
                        captures.push((data.len(), bits.len()));
                    }
                    data.extend_from_bitslice(bits.as_bits());
                    index += 1;
                }
                let end = match ops.get(index) {
                    Some((_, JtagOp::EnterState(end))) => {
                        index += 1;
                        *end
                    }
                    _ => state,
                };

                let bits = data.len();
                if bits == 0 {
                    steps.extend(move_to(state, end));
                    state = end;
                    continue;
                }
                let bytes = bits.div_ceil(8);
                if bits > u16::MAX as usize || 4 + bytes + 2 > MAX_DATA_BLOCK {
                    return Err(DebugProbeError::Other(format!(
                        "JTAG scan of {bits} bits is too long for the XDS110"
                    )));
                }
                let mut entry = vec![command, XdsState::from(end) as u8];
                entry.extend_from_slice(&(bits as u16).to_le_bytes());
                data.set_uninitialized(false);
                entry.extend_from_slice(data.as_raw_slice());
                steps.push(Step::Scan {
                    entry,
                    captures,
                    bits,
                });
                state = end;
            }
            JtagOp::ClockTck { count } => {
                let step = if state == TapState::RunTestIdle {
                    run_test(*count, state)
                } else {
                    let mut entry = vec![CMD_STABLECLOCKS];
                    entry.extend_from_slice(&count.to_le_bytes());
                    Step::Scan {
                        entry,
                        captures: vec![],
                        bits: 0,
                    }
                };
                steps.push(step);
                index += 1;
            }
        }
    }

    Ok((state, steps))
}

/// Scans queued for one `OCD_SCAN_REQUEST`.
#[derive(Default)]
struct ScanRequest {
    entries: Vec<u8>,
    /// (captures, length in bits) of each scan.
    scans: Vec<(Vec<(usize, usize)>, usize)>,
    result_bytes: usize,
}

impl Xds110 {
    fn run_scan_request(
        &mut self,
        request: &mut ScanRequest,
        captured: &mut BitVec,
    ) -> Result<(), DebugProbeError> {
        if request.entries.is_empty() {
            return Ok(());
        }
        let mut payload = Vec::with_capacity(request.entries.len() + 2);
        payload.push(OCD_SCAN_REQUEST);
        payload.append(&mut request.entries);
        payload.push(0);
        let response = self.usb.execute(&payload, request.result_bytes)?;

        let bits = BitVec::<u8>::from_slice(&response);
        let mut offset = 0;
        for (captures, length) in request.scans.drain(..) {
            for (start, len) in captures {
                captured.extend(bits[offset + start..offset + start + len].iter().by_vals());
            }
            offset += length.div_ceil(8) * 8;
        }
        request.result_bytes = 0;
        Ok(())
    }
}

impl JtagChainAccess for Xds110 {
    fn chain_state(&mut self) -> &mut JtagChainState {
        &mut self.jtag_state
    }

    fn chain_state_ref(&self) -> &JtagChainState {
        &self.jtag_state
    }
}

impl JtagProbe for Xds110 {
    fn run_batch(
        &mut self,
        batch: &JtagBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        let failed = |error| BatchExecutionError::new_from_debug_probe(error, Results::new());
        let (state, steps) = encode_batch(self.jtag_state.tap_state, batch).map_err(failed)?;
        self.tms_state = None;

        let mut request = ScanRequest::default();
        let mut captured = BitVec::new();
        for step in steps {
            match step {
                Step::Scan {
                    entry,
                    captures,
                    bits,
                } => {
                    let bytes = bits.div_ceil(8);
                    if request.entries.len() + entry.len() + 1 > MAX_DATA_BLOCK
                        || request.result_bytes + bytes > MAX_DATA_BLOCK
                    {
                        self.run_scan_request(&mut request, &mut captured)
                            .map_err(failed)?;
                    }
                    request.entries.extend_from_slice(&entry);
                    if bits > 0 {
                        request.scans.push((captures, bits));
                        request.result_bytes += bytes;
                    }
                }
                Step::Path(path) => {
                    self.run_scan_request(&mut request, &mut captured)
                        .map_err(failed)?;
                    let mut payload = vec![super::OCD_PATHMOVE];
                    payload.extend_from_slice(&(path.len() as u32).to_le_bytes());
                    payload.extend_from_slice(&path);
                    self.command(&payload).map_err(failed)?;
                }
            }
        }
        self.run_scan_request(&mut request, &mut captured)
            .map_err(failed)?;
        self.jtag_state.tap_state = state;

        distribute_captures(batch.iter(), &captured, Results::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::BitSequence;

    #[test]
    fn a_dr_scan_ends_in_the_next_state() {
        let mut batch = JtagBatch::new();
        batch.enter(TapState::ShiftDr);
        let _handle = batch.exchange(BitSequence::from_u64(35, 0x1_2345_6789));
        batch.enter(TapState::RunTestIdle);

        let (state, steps) = encode_batch(TapState::RunTestIdle, &batch).unwrap();
        assert_eq!(state, TapState::RunTestIdle);
        assert_eq!(
            steps,
            vec![Step::Scan {
                entry: vec![CMD_DR_SCAN, 2, 35, 0, 0x89, 0x67, 0x45, 0x23, 0x01],
                captures: vec![(0, 35)],
                bits: 35,
            }]
        );
    }

    #[test]
    fn neighbour_exchanges_make_one_scan() {
        let mut batch = JtagBatch::new();
        batch.enter(TapState::ShiftIr);
        batch.exchange_no_capture(BitSequence::from_u64(4, 0xf));
        let _handle = batch.exchange(BitSequence::from_u64(6, 0x04));
        batch.enter(TapState::PauseIr);

        let (state, steps) = encode_batch(TapState::RunTestIdle, &batch).unwrap();
        assert_eq!(state, TapState::PauseIr);
        assert_eq!(
            steps,
            vec![Step::Scan {
                entry: vec![CMD_IR_SCAN, 6, 10, 0, 0x4f, 0x00],
                captures: vec![(4, 6)],
                bits: 10,
            }]
        );
    }

    #[test]
    fn a_move_between_pause_states_is_a_path() {
        let mut batch = JtagBatch::new();
        batch.enter(TapState::PauseIr);

        let (_, steps) = encode_batch(TapState::PauseDr, &batch).unwrap();
        use XdsState::{CaptureIr, Exit1Ir, Exit2Dr, PauseIr, SelectDr, SelectIr, UpdateDr};
        assert_eq!(
            steps,
            vec![Step::Path(
                [
                    Exit2Dr, UpdateDr, SelectDr, SelectIr, CaptureIr, Exit1Ir, PauseIr
                ]
                .map(|s| s as u8)
                .to_vec()
            )]
        );
    }

    #[test]
    fn an_empty_scan_is_a_move() {
        let mut batch = JtagBatch::new();
        batch.enter(TapState::ShiftIr);
        let _handle = batch.exchange(BitSequence::new());
        batch.enter(TapState::RunTestIdle);

        let (state, steps) = encode_batch(TapState::RunTestIdle, &batch).unwrap();
        assert_eq!(state, TapState::RunTestIdle);
        assert!(
            steps
                .iter()
                .all(|step| !matches!(step, Step::Scan { bits, .. } if *bits > 0))
        );
        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, Step::Scan { entry, .. } if entry[0] == CMD_IR_SCAN))
        );
    }

    #[test]
    fn clocks_in_idle_are_run_test() {
        let mut batch = JtagBatch::new();
        batch.enter(TapState::RunTestIdle);
        batch.clock(100);

        let (_, steps) = encode_batch(TapState::TestLogicReset, &batch).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1], run_test(100, TapState::RunTestIdle));
    }
}
