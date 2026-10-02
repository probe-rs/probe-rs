use bitvec::{bitvec, slice::BitSlice, vec::BitVec};

use crate::probe::{
    Batch, BatchExecutionError, DebugProbeError, JtagChainAccess, JtagChainState, JtagOp,
    JtagProbe, Results,
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
/// The sequence count of a `DAP_JTAG_Sequence` request is one byte.
const MAX_SEQUENCES: usize = u8::MAX as usize;

impl JtagChainAccess for CmsisDap {
    fn chain_state(&mut self) -> &mut JtagChainState {
        &mut self.jtag_state
    }

    fn chain_state_ref(&self) -> &JtagChainState {
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

    fn configure_jtag(&mut self, skip_scan: bool) -> Result<(), DebugProbeError> {
        CmsisDap::configure_jtag(self, skip_scan)
    }
}

impl CmsisDap {
    fn run_jtag_batch(
        &mut self,
        start: TapState,
        batch: &Batch<JtagOp, DebugProbeError>,
    ) -> Result<(TapState, Results), BatchExecutionError<DebugProbeError>> {
        let failed = |error| BatchExecutionError::new_from_debug_probe(error, Results::new());
        let (state, requests) =
            encode_jtag_batch(self.packet_size - 1, start, batch).map_err(failed)?;
        let mut captured = BitVec::new();
        for request in requests {
            let response = self
                .send_jtag_sequences(request)
                .map_err(|error| failed(error.into()))?;
            captured.extend_from_bitslice(&response);
        }
        let results = distribute_captures(batch.iter(), &captured, Results::new())?;
        Ok((state, results))
    }
}

/// Encodes a batch that starts with the TAP in `start` into `DAP_JTAG_Sequence` requests that
/// each fit in `packet_size` bytes.
fn encode_jtag_batch(
    packet_size: u16,
    start: TapState,
    batch: &Batch<JtagOp, DebugProbeError>,
) -> Result<(TapState, Vec<SequenceRequest>), DebugProbeError> {
    let ops: Vec<_> = batch.iter().collect();
    let mut state = start;
    let mut skip_enter_path_bits = 0usize;
    let mut buffer = JtagBuffer::new(packet_size);
    let mut requests = Vec::new();

    for (index, (id, op)) in ops.iter().enumerate() {
        match op {
            JtagOp::EnterState(target) => {
                let target = *target;
                let path = &state.path_to(target)[skip_enter_path_bits..];
                skip_enter_path_bits = 0;
                let tdi = enter_tdi(target);
                for (tms, run_length) in tms_runs(path) {
                    let data = BitVec::repeat(tdi, run_length);
                    buffer.push_into(tms, &data, false, &mut requests)?;
                }
                state = target;
            }
            JtagOp::Exchange { data, capture } => {
                if state != TapState::ShiftIr && state != TapState::ShiftDr {
                    return Err(DebugProbeError::Other(format!(
                        "Exchange in state {state:?}, but ShiftIr or ShiftDr is required"
                    )));
                }
                let do_capture = *capture && id.should_capture();
                let bit_count = data.len();
                // Avoid panicking if there is no data.
                let merge_exit = bit_count > 0
                    && exchange_leaves_shift(state, ops.get(index + 1).map(|(_, op)| op));
                let data_bits = if merge_exit { bit_count - 1 } else { bit_count };
                let mut offset = 0;
                while offset < data_bits {
                    let chunk = (data_bits - offset).min(MAX_SEQUENCE_BITS);
                    let mut sequence_data = BitVec::new();
                    for bit_index in 0..chunk {
                        sequence_data.push(data[offset + bit_index]);
                    }
                    buffer.push_into(false, &sequence_data, do_capture, &mut requests)?;
                    offset += chunk;
                }
                if merge_exit {
                    skip_enter_path_bits = 1;
                    let last_tdi = data[bit_count - 1];
                    let sequence_data = bitvec![last_tdi as usize; 1];
                    buffer.push_into(true, &sequence_data, do_capture, &mut requests)?;
                }
            }
            JtagOp::ClockTck { count } => {
                let mut remaining = *count as usize;
                while remaining > 0 {
                    let chunk = remaining.min(MAX_SEQUENCE_BITS);
                    let data = BitVec::repeat(false, chunk);
                    buffer.push_into(false, &data, false, &mut requests)?;
                    remaining -= chunk;
                }
            }
        }
    }

    requests.extend(buffer.take_request()?);
    Ok((state, requests))
}

struct BufferedJtagSequence {
    tdo_capture: bool,
    tms: bool,
    data: BitVec,
}

impl BufferedJtagSequence {
    /// Returns the size of the sequence in bytes.
    fn size(&self) -> usize {
        1 + self.data.len().div_ceil(8)
    }

    fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

struct JtagBuffer {
    packet_size: usize,
    current_sequence: Option<BufferedJtagSequence>,
    complete_sequences: Vec<BufferedJtagSequence>,
}

impl JtagBuffer {
    fn new(packet_size: u16) -> Self {
        Self {
            packet_size: packet_size as usize,
            current_sequence: None,
            complete_sequences: Vec::with_capacity(packet_size as usize),
        }
    }

    /// Pushes a sequence, and moves the buffer into `requests` when it is full.
    fn push_into(
        &mut self,
        tms: bool,
        data: &BitSlice,
        tdo_capture: bool,
        requests: &mut Vec<SequenceRequest>,
    ) -> Result<(), DebugProbeError> {
        self.push_sequence(tms, data, tdo_capture)?;
        if self.should_flush() {
            requests.extend(self.take_request()?);
        }
        Ok(())
    }

    /// Takes the buffered sequences as one request, if there are any.
    fn take_request(&mut self) -> Result<Option<SequenceRequest>, DebugProbeError> {
        if let Some(seq) = self.current_sequence.take()
            && !seq.is_empty()
        {
            self.complete_sequences.push(seq);
        }

        if self.complete_sequences.is_empty() {
            return Ok(None);
        }

        let sequences = self
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

        Ok(Some(SequenceRequest::new(sequences)?))
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

    fn push_sequence(
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

        self.current_sequence = Some(BufferedJtagSequence {
            tdo_capture,
            tms,
            data: data.to_bitvec(),
        });
        Ok(())
    }

    fn should_flush(&self) -> bool {
        // Checked after each push, so leave room for one more full sequence.
        let sequences =
            self.complete_sequences.len() + usize::from(self.current_sequence.is_some());
        self.total_buffer_bytes() + MAX_SEQUENCE_BITS.div_ceil(8) > self.packet_size
            || sequences >= MAX_SEQUENCES
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
mod tests {
    use super::{JtagBuffer, MAX_SEQUENCE_BITS};
    use crate::probe::cmsisdap::commands::Request;
    use bitvec::vec::BitVec;

    /// Takes the buffered request, serializes it into `packet_size` bytes, and returns the
    /// number of bytes written. Building the request checks the sequence count.
    fn flush(buffer: &mut JtagBuffer, packet_size: usize) -> usize {
        buffer.take_request().unwrap().map_or(0, |request| {
            request.to_bytes(&mut vec![0u8; packet_size]).unwrap()
        })
    }

    /// `should_flush` is checked after each push, so it must trigger while one
    /// more full sequence still fits, and before the request holds more
    /// sequences than its count byte can describe. Mixed widths matter: uniform
    /// widths pass at some packet sizes by luck.
    #[test]
    fn flushed_buffer_fits_packet() {
        for packet_size in [64u16, 128, 256, 512, 1024] {
            for widths in [
                [MAX_SEQUENCE_BITS, MAX_SEQUENCE_BITS],
                [1, 1],
                [1, MAX_SEQUENCE_BITS],
                [32, MAX_SEQUENCE_BITS],
            ] {
                let mut buffer = JtagBuffer::new(packet_size);
                for i in 0..400 {
                    // Alternating TMS keeps each push a separate sequence.
                    let tms = i % 2 == 0;
                    let data = BitVec::repeat(tms, widths[i % 2]);
                    buffer.push_sequence(tms, &data, false).unwrap();
                    if buffer.should_flush() {
                        let written = flush(&mut buffer, packet_size as usize);
                        assert!(
                            written <= packet_size as usize,
                            "{packet_size}-byte packet, widths {widths:?}: wrote {written}"
                        );
                    }
                }
                let written = flush(&mut buffer, packet_size as usize);
                assert!(
                    written <= packet_size as usize,
                    "{packet_size}-byte packet, widths {widths:?}: wrote {written} at the end"
                );
            }
        }
    }
}

#[cfg(test)]
mod golden_tests {
    use super::decoder::decode_requests;
    use super::encode_jtag_batch;
    use crate::probe::BitSequence;
    use crate::probe::cmsisdap::commands::Request;
    use crate::probe::jtag::golden::{
        SHIFT_IR_ONE_TAP, SHIFT_IR_THREE_TAP, assert_triples_eq, build_ir_exchange, lowering_batch,
        move_literal, one_tap_params, three_tap_params,
    };
    use crate::probe::jtag::{JtagBatch, TapState};

    fn collect_cmsis_requests(
        packet_size: u16,
        start: TapState,
        batch: &JtagBatch,
    ) -> (TapState, Vec<Vec<u8>>) {
        let (state, requests) = encode_jtag_batch(packet_size, start, batch).unwrap();
        let requests = requests
            .iter()
            .map(|request| {
                let mut bytes = vec![0u8; usize::from(packet_size)];
                let length = request.to_bytes(&mut bytes).unwrap();
                bytes.truncate(length);
                bytes
            })
            .collect();
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
    fn a_captured_scan_captures_every_bit_including_the_exit_bit() {
        let mut batch = JtagBatch::new();
        batch.enter(TapState::ShiftDr);
        let _handle = batch.exchange(BitSequence::repeat(true, 41));
        batch.enter(TapState::RunTestIdle);
        let (_, requests) = collect_cmsis_requests(64, TapState::RunTestIdle, &batch);
        let triples = decode_requests(&requests);
        let captured: Vec<_> = triples.iter().map(|(_, _, capture)| *capture).collect();
        let mut expected = vec![false; 3];
        expected.extend([true; 41]);
        expected.extend([false; 2]);
        assert_eq!(captured, expected);
    }

    #[test]
    fn many_short_scans_split_at_the_sequence_limit() {
        // Each scan alternates TMS a few times, so the sequences are short and a 1024-byte
        // packet would hold more of them than a request can count.
        let mut batch = JtagBatch::new();
        for _ in 0..300 {
            batch.enter(TapState::ShiftDr);
            batch.exchange_no_capture(BitSequence::from_u64(2, 0b01));
            batch.enter(TapState::RunTestIdle);
        }
        let (_, requests) = collect_cmsis_requests(1024, TapState::RunTestIdle, &batch);
        assert!(requests.len() > 1);
        assert_eq!(
            decode_requests(&requests),
            lowering_batch(TapState::RunTestIdle, &batch)
        );
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
