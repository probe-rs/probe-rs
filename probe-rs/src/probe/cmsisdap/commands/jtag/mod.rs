use bitvec::{bitvec, slice::BitSlice, vec::BitVec};

use crate::probe::{
    Batch, BatchExecutionError, DebugProbeError, JtagDriverState, JtagOp, JtagProbe, JtagSequence,
    JtagStateAccess, Results,
    cmsisdap::{
        CmsisDap,
        commands::jtag::sequence::{Sequence, SequenceRequest},
    },
    jtag::{TapState, distribute_captures, enter_tdi, exchange_leaves_shift, tms_runs},
};

pub mod configure;
pub mod idcode;
pub mod sequence;

const MAX_SEQUENCE_BITS: usize = 64;

impl JtagStateAccess for CmsisDap {
    fn state_mut(&mut self) -> &mut JtagDriverState {
        &mut self.jtag_state
    }

    fn state(&self) -> &JtagDriverState {
        &self.jtag_state
    }
}

impl JtagProbe for CmsisDap {
    fn run_batch(
        &mut self,
        batch: &Batch<JtagOp, DebugProbeError>,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        let start = self.jtag_state.tap_state;
        let (state, results) = self.run_jtag_batch(start, batch)?;
        self.jtag_state.tap_state = state;
        Ok(results)
    }

    fn shift_raw_sequence(&mut self, sequence: JtagSequence) -> Result<BitVec, DebugProbeError> {
        self.jtag_buffer.complete_sequences.clear();
        self.jtag_buffer.current_sequence = None;
        self.jtag_buffer.response.clear();

        self.jtag_buffer
            .push_sequence(sequence.tms, &sequence.data, sequence.tdo_capture)?;
        self.flush_jtag()?;
        if sequence.tdo_capture {
            Ok(std::mem::take(&mut self.jtag_buffer.response))
        } else {
            self.jtag_buffer.response.clear();
            Ok(BitVec::new())
        }
    }
}

impl CmsisDap {
    fn flush_jtag(&mut self) -> Result<(), DebugProbeError> {
        if let Some(seq) = self.jtag_buffer.current_sequence.take()
            && !seq.is_empty()
        {
            self.jtag_buffer.complete_sequences.push(seq);
        }

        if self.jtag_buffer.complete_sequences.is_empty() {
            return Ok(());
        }

        let sequences = self
            .jtag_buffer
            .complete_sequences
            .drain(..)
            .map(|s| {
                if s.tdo_capture {
                    Sequence::capture(s.tms, &s.data)
                } else {
                    Sequence::no_capture(s.tms, &s.data)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        let command = SequenceRequest::new(sequences)?;

        let response = self.send_jtag_sequences(command)?;
        self.jtag_buffer.response.extend_from_bitslice(&response);

        Ok(())
    }

    fn run_jtag_batch(
        &mut self,
        start: TapState,
        batch: &Batch<JtagOp, DebugProbeError>,
    ) -> Result<(TapState, Results), BatchExecutionError<DebugProbeError>> {
        let ops: Vec<_> = batch.iter().collect();
        let mut state = start;
        let results = Results::new();
        let mut skip_enter_path_bits = 0usize;

        for (index, (id, op)) in ops.iter().enumerate() {
            match op {
                JtagOp::EnterState(target) => {
                    let target = *target;
                    let path = &state.path_to(target)[skip_enter_path_bits..];
                    skip_enter_path_bits = 0;
                    let tdi = enter_tdi(target);
                    for (tms, run_length) in tms_runs(path) {
                        let data = BitVec::repeat(tdi, run_length);
                        if let Err(error) = self.jtag_buffer.push_sequence(tms, &data, false) {
                            return Err(BatchExecutionError::new_from_debug_probe(error, results));
                        }
                        if self.jtag_buffer.should_flush()
                            && let Err(error) = self.flush_jtag()
                        {
                            return Err(BatchExecutionError::new_from_debug_probe(error, results));
                        }
                    }
                    state = target;
                }
                JtagOp::Exchange { data, capture } => {
                    if state != TapState::ShiftIr && state != TapState::ShiftDr {
                        return Err(BatchExecutionError::new_from_debug_probe(
                            DebugProbeError::Other(format!(
                                "Exchange in state {state:?}, but ShiftIr or ShiftDr is required"
                            )),
                            results,
                        ));
                    }
                    let merge_exit =
                        exchange_leaves_shift(state, ops.get(index + 1).map(|(_, op)| op));
                    let do_capture = *capture && id.should_capture();
                    let bit_count = data.len();
                    let data_bits = if merge_exit { bit_count - 1 } else { bit_count };
                    let mut offset = 0;
                    while offset < data_bits {
                        let chunk = (data_bits - offset).min(MAX_SEQUENCE_BITS);
                        let mut sequence_data = BitVec::new();
                        for bit_index in 0..chunk {
                            sequence_data.push(data[offset + bit_index]);
                        }
                        if let Err(error) =
                            self.jtag_buffer
                                .push_sequence(false, &sequence_data, do_capture)
                        {
                            return Err(BatchExecutionError::new_from_debug_probe(error, results));
                        }
                        if self.jtag_buffer.should_flush()
                            && let Err(error) = self.flush_jtag()
                        {
                            return Err(BatchExecutionError::new_from_debug_probe(error, results));
                        }
                        offset += chunk;
                    }
                    if merge_exit {
                        skip_enter_path_bits = 1;
                        let last_tdi = data[bit_count - 1];
                        let sequence_data = bitvec![last_tdi as usize; 1];
                        if let Err(error) =
                            self.jtag_buffer.push_sequence(true, &sequence_data, false)
                        {
                            return Err(BatchExecutionError::new_from_debug_probe(error, results));
                        }
                        if self.jtag_buffer.should_flush()
                            && let Err(error) = self.flush_jtag()
                        {
                            return Err(BatchExecutionError::new_from_debug_probe(error, results));
                        }
                    }
                }
                JtagOp::ClockTck { count } => {
                    let mut remaining = *count as usize;
                    while remaining > 0 {
                        let chunk = remaining.min(MAX_SEQUENCE_BITS);
                        let data = BitVec::repeat(false, chunk);
                        if let Err(error) = self.jtag_buffer.push_sequence(false, &data, false) {
                            return Err(BatchExecutionError::new_from_debug_probe(error, results));
                        }
                        if self.jtag_buffer.should_flush()
                            && let Err(error) = self.flush_jtag()
                        {
                            return Err(BatchExecutionError::new_from_debug_probe(error, results));
                        }
                        remaining -= chunk;
                    }
                }
            }
        }

        if let Err(error) = self.flush_jtag() {
            return Err(BatchExecutionError::new_from_debug_probe(error, results));
        }

        let captured = std::mem::take(&mut self.jtag_buffer.response);
        let results = distribute_captures(ops, &captured, results)?;

        Ok((state, results))
    }
}

impl JtagSequence {
    /// Returns the size of the sequence in bytes.
    fn size(&self) -> usize {
        1 + self.data.len().div_ceil(8)
    }

    fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

pub(crate) struct JtagBuffer {
    packet_size: usize,
    pub(crate) current_sequence: Option<JtagSequence>,
    pub(crate) complete_sequences: Vec<JtagSequence>,
    response: BitVec,
}

impl JtagBuffer {
    pub(crate) fn new(packet_size: u16) -> Self {
        Self {
            packet_size: packet_size as usize,
            current_sequence: None,
            complete_sequences: Vec::with_capacity(packet_size as usize),
            response: BitVec::with_capacity(packet_size as usize),
        }
    }

    fn total_buffer_bytes(&self) -> usize {
        1 + 1
            + self
                .complete_sequences
                .iter()
                .map(|s| s.size())
                .sum::<usize>()
            + self.current_sequence.as_ref().map_or(0, |s| s.size())
    }

    pub(crate) fn push_sequence(
        &mut self,
        tms: bool,
        data: &BitSlice,
        tdo_capture: bool,
    ) -> Result<(), DebugProbeError> {
        if data.is_empty() {
            return Ok(());
        }

        if let Some(seq) = &mut self.current_sequence {
            if seq.tms == tms
                && seq.tdo_capture == tdo_capture
                && seq.data.len() + data.len() <= MAX_SEQUENCE_BITS
            {
                seq.data.extend_from_bitslice(data);
                return Ok(());
            }
            let complete = self.current_sequence.take().expect("sequence present");
            self.complete_sequences.push(complete);
        }

        self.current_sequence = Some(JtagSequence {
            tdo_capture,
            tms,
            data: data.to_bitvec(),
        });
        Ok(())
    }

    pub(crate) fn should_flush(&self) -> bool {
        self.total_buffer_bytes() >= self.packet_size - 1
    }
}

#[cfg(test)]
pub(crate) mod decoder {
    use bitvec::order::Lsb0;
    use bitvec::view::BitView;

    pub(crate) fn decode_request(bytes: &[u8]) -> Vec<(bool, bool, bool)> {
        let mut triples = Vec::new();
        let sequence_count = bytes[0] as usize;
        let mut index = 1;
        for _ in 0..sequence_count {
            let info = bytes[index];
            index += 1;
            let tck_cycles = usize::from(if info & 0x3f == 0 { 64 } else { info & 0x3f });
            let tms = info & 0x40 != 0;
            let capture = info & 0x80 != 0;
            let byte_count = tck_cycles.div_ceil(8);
            let tdi_bits = bytes[index..index + byte_count].view_bits::<Lsb0>();
            for bit_index in 0..tck_cycles {
                triples.push((tms, tdi_bits[bit_index], capture));
            }
            index += byte_count;
        }
        triples
    }

    pub(crate) fn decode_requests(requests: &[Vec<u8>]) -> Vec<(bool, bool, bool)> {
        let mut triples = Vec::new();
        for request in requests {
            triples.extend(decode_request(request));
        }
        triples
    }
}

#[cfg(test)]
mod golden_tests {
    use super::decoder::decode_requests;
    use super::{JtagBuffer, MAX_SEQUENCE_BITS, enter_tdi, exchange_leaves_shift, tms_runs};
    use crate::probe::cmsisdap::commands::Request;
    use crate::probe::cmsisdap::commands::jtag::sequence::{Sequence, SequenceRequest};
    use crate::probe::jtag::golden::{
        SHIFT_IR_ONE_TAP, SHIFT_IR_THREE_TAP, assert_triples_eq, build_ir_exchange, move_literal,
        one_tap_params, three_tap_params,
    };
    use crate::probe::jtag::{JtagBatch, TapState};
    use crate::probe::{Batch, BitSequence, DebugProbeError, JtagOp};
    use bitvec::{bitvec, vec::BitVec};

    fn collect_cmsis_requests(
        packet_size: usize,
        start: TapState,
        batch: &Batch<JtagOp, DebugProbeError>,
    ) -> (TapState, Vec<Vec<u8>>) {
        let ops: Vec<_> = batch.iter().collect();
        let mut state = start;
        let mut skip_enter_path_bits = 0usize;
        let mut jtag_buffer = JtagBuffer::new(packet_size as u16);
        let mut requests = Vec::new();

        let mut flush = |buffer: &mut JtagBuffer| {
            if let Some(seq) = buffer.current_sequence.take()
                && !seq.is_empty()
            {
                buffer.complete_sequences.push(seq);
            }
            if buffer.complete_sequences.is_empty() {
                return;
            }
            let sequences = buffer
                .complete_sequences
                .drain(..)
                .map(|s| {
                    if s.tdo_capture {
                        Sequence::capture(s.tms, &s.data)
                    } else {
                        Sequence::no_capture(s.tms, &s.data)
                    }
                })
                .collect::<Result<Vec<_>, _>>()
                .expect("sequence encoding failed");
            let request = SequenceRequest::new(sequences).expect("request encoding failed");
            let mut bytes = vec![0u8; 4096];
            let length = request.to_bytes(&mut bytes).expect("request bytes failed");
            requests.push(bytes[..length].to_vec());
        };

        for (index, (_id, op)) in ops.iter().enumerate() {
            match op {
                JtagOp::EnterState(target) => {
                    let target = *target;
                    let path = &state.path_to(target)[skip_enter_path_bits..];
                    skip_enter_path_bits = 0;
                    let tdi = enter_tdi(target);
                    for (tms, run_length) in tms_runs(path) {
                        let data = BitVec::repeat(tdi, run_length);
                        jtag_buffer
                            .push_sequence(tms, &data, false)
                            .expect("push failed");
                        if jtag_buffer.should_flush() {
                            flush(&mut jtag_buffer);
                        }
                    }
                    state = target;
                }
                JtagOp::Exchange { data, capture: _ } => {
                    let merge_exit =
                        exchange_leaves_shift(state, ops.get(index + 1).map(|(_, op)| op));
                    let bit_count = data.len();
                    let data_bits = if merge_exit { bit_count - 1 } else { bit_count };
                    let mut offset = 0;
                    while offset < data_bits {
                        let chunk = (data_bits - offset).min(MAX_SEQUENCE_BITS);
                        let mut sequence_data = BitVec::new();
                        for bit_index in 0..chunk {
                            sequence_data.push(data[offset + bit_index]);
                        }
                        jtag_buffer
                            .push_sequence(false, &sequence_data, false)
                            .expect("push failed");
                        if jtag_buffer.should_flush() {
                            flush(&mut jtag_buffer);
                        }
                        offset += chunk;
                    }
                    if merge_exit {
                        skip_enter_path_bits = 1;
                        let last_tdi = data[bit_count - 1];
                        let sequence_data = bitvec![last_tdi as usize; 1];
                        jtag_buffer
                            .push_sequence(true, &sequence_data, false)
                            .expect("push failed");
                        if jtag_buffer.should_flush() {
                            flush(&mut jtag_buffer);
                        }
                    }
                }
                JtagOp::ClockTck { count } => {
                    let mut remaining = *count as usize;
                    while remaining > 0 {
                        let chunk = remaining.min(MAX_SEQUENCE_BITS);
                        let data = BitVec::repeat(false, chunk);
                        jtag_buffer
                            .push_sequence(false, &data, false)
                            .expect("push failed");
                        if jtag_buffer.should_flush() {
                            flush(&mut jtag_buffer);
                        }
                        remaining -= chunk;
                    }
                }
            }
        }

        flush(&mut jtag_buffer);
        (state, requests)
    }

    #[test]
    fn move_to_state_matches_golden() {
        const STABLE_STATES: [TapState; 6] = [
            TapState::TestLogicReset,
            TapState::RunTestIdle,
            TapState::ShiftIr,
            TapState::ShiftDr,
            TapState::PauseIr,
            TapState::PauseDr,
        ];

        for from in STABLE_STATES {
            for to in STABLE_STATES {
                if to == TapState::TestLogicReset {
                    continue;
                }
                let mut batch = JtagBatch::new();
                batch.enter(to);
                let (_, requests) = collect_cmsis_requests(64, from, &batch);
                let triples = decode_requests(&requests);
                let (tms, tdi, cap) = move_literal(from, to);
                assert_triples_eq(&triples, tms, tdi, cap);
            }
        }
    }

    #[test]
    fn shift_ir_matches_golden() {
        for (literal, params) in [
            (SHIFT_IR_ONE_TAP, one_tap_params()),
            (SHIFT_IR_THREE_TAP, three_tap_params()),
        ] {
            let mut batch = JtagBatch::new();
            batch.enter(TapState::ShiftIr);
            batch.exchange_no_capture(build_ir_exchange(params, 0b10110, 5));
            batch.enter(TapState::RunTestIdle);
            let (_, requests) = collect_cmsis_requests(64, TapState::TestLogicReset, &batch);
            let triples = decode_requests(&requests);
            assert_triples_eq(&triples, literal.0, literal.1, literal.2);
        }
    }

    #[test]
    fn large_exchange_crosses_packet_boundary() {
        let mut batch = JtagBatch::new();
        batch.enter(TapState::ShiftDr);
        batch.exchange_no_capture(BitSequence::repeat(false, 4096));
        let (state, requests) = collect_cmsis_requests(64, TapState::TestLogicReset, &batch);
        assert_eq!(state, TapState::ShiftDr);
        assert!(requests.len() > 1);
    }
}
