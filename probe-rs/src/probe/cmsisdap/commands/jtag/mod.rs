use bitvec::{bitvec, vec::BitVec};

use crate::probe::{
    DebugProbeError, JtagDriverState, JtagSequence, RawJtagIo,
    cmsisdap::{
        CmsisDap,
        commands::jtag::sequence::{Sequence, SequenceRequest},
    },
};

pub mod configure;
pub mod idcode;
pub mod sequence;

// Implement everything necessary to bitbang JTAG for non-ARM interfaces. As this is
// mutually exclusive with ARM JTAG, we can ignore the ARM-side batching implementation.
impl RawJtagIo for CmsisDap {
    fn shift_bit(
        &mut self,
        tms: bool,
        tdi: bool,
        capture_tdo: bool,
    ) -> Result<(), DebugProbeError> {
        self.state_mut().state.update(tms);
        if self.jtag_buffer.shift_bit(tms, tdi, capture_tdo) {
            self.flush_jtag()?;
        }
        Ok(())
    }

    fn read_captured_bits(&mut self) -> Result<BitVec, DebugProbeError> {
        self.flush_jtag()?;
        Ok(std::mem::take(&mut self.jtag_buffer.response))
    }

    fn state_mut(&mut self) -> &mut JtagDriverState {
        &mut self.jtag_state
    }

    fn state(&self) -> &JtagDriverState {
        &self.jtag_state
    }
}

impl CmsisDap {
    fn flush_jtag(&mut self) -> Result<(), DebugProbeError> {
        if let Some(seq) = self.jtag_buffer.current_sequence.take() {
            if !seq.is_empty() {
                self.jtag_buffer.complete_sequences.push(seq);
            }
        }

        // Flush was called but not neeed.
        if self.jtag_buffer.complete_sequences.is_empty() {
            return Ok(());
        }

        // Transform into sequence::Sequence
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
}

impl JtagSequence {
    /// Returns the size of the sequence in bytes.
    fn size(&self) -> usize {
        // Sequence info + TDI data
        1 + self.data.len().div_ceil(8)
    }

    fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    fn append(&mut self, tdi: bool, tms: bool, tdo_capture: bool) -> Option<JtagSequence> {
        if tms == self.tms && tdo_capture == self.tdo_capture && self.data.len() < 64 {
            self.data.push(tdi);
            None
        } else {
            let seq = std::mem::replace(
                self,
                JtagSequence {
                    tdo_capture,
                    tms,
                    data: bitvec![tdi as usize; 1],
                },
            );
            Some(seq)
        }
    }
}

pub(crate) struct JtagBuffer {
    packet_size: usize,
    current_sequence: Option<JtagSequence>,
    complete_sequences: Vec<JtagSequence>,
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
}
impl JtagBuffer {
    fn total_buffer_bytes(&self) -> usize {
        // Command + Sequence count + (Sequence info + data bytes)+
        1 + 1
            + self
                .complete_sequences
                .iter()
                .map(|s| s.size())
                .sum::<usize>()
            + self.current_sequence.as_ref().map_or(0, |s| s.size())
    }

    /// Returns `true` if the buffered sequences need to be flushed.
    fn shift_bit(&mut self, tms: bool, tdi: bool, tdo_capture: bool) -> bool {
        let seq = self.current_sequence.get_or_insert_with(|| JtagSequence {
            tdo_capture,
            tms,
            data: BitVec::with_capacity(64),
        });

        if let Some(complete_sequence) = seq.append(tdi, tms, tdo_capture) {
            self.complete_sequences.push(complete_sequence);
        }

        self.should_flush()
    }

    fn should_flush(&self) -> bool {
        // In the worst case, the next bit will append two bytes. If we only have 1 byte left,
        // we need to flush the buffer.
        self.total_buffer_bytes() >= self.packet_size - 1
    }
}
