use bitvec::{order::Lsb0, vec::BitVec};

use crate::probe::{
    DebugProbeError,
    cmsisdap::{
        CmsisDap,
        commands::jtag::sequence::{Sequence, SequenceRequest},
    },
    common::{JtagDriverState, RawJtagIo},
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

    fn read_captured_bits(&mut self) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
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
            self.jtag_buffer.complete_sequences.push(seq);
        }

        // Transform into sequence::Sequence
        let sequences = self
            .jtag_buffer
            .complete_sequences
            .drain(..)
            .map(|s| Sequence::new(s.tck_cycles, s.tdo_capture, s.tms, to_bytes(&s.data)))
            .collect::<Result<Vec<_>, _>>()?;

        let command = SequenceRequest::new(sequences)?;

        let response = self.send_jtag_sequences(command)?;
        self.jtag_buffer.response.extend_from_bitslice(&response);

        Ok(())
    }
}

fn to_bytes(data: &[bool]) -> [u8; 8] {
    let mut bytes = [0; 8];
    for (i, &bit) in data.iter().enumerate() {
        if bit {
            bytes[i / 8] |= 1 << (i % 8);
        }
    }
    bytes
}

struct JtagSequence {
    /// Number of TCK cycles: 1..64 (64 encoded as 0)
    tck_cycles: u8,

    /// TDO capture
    tdo_capture: bool,

    /// TMS value
    tms: bool,

    /// Data to generate on TDI
    data: Vec<bool>,
}

impl JtagSequence {
    /// Returns the size of the sequence in bytes.
    fn size(&self) -> usize {
        // Sequence info + TDI data
        1 + self.data.len().div_ceil(8)
    }

    fn append(&mut self, tdi: bool, tms: bool, tdo_capture: bool) -> Option<JtagSequence> {
        if tms == self.tms && tdo_capture == self.tdo_capture && self.tck_cycles < 64 {
            self.data.push(tdi);
            self.tck_cycles += 1;
            None
        } else {
            let seq = std::mem::replace(
                self,
                JtagSequence {
                    tck_cycles: 1,
                    tdo_capture,
                    tms,
                    data: vec![tdi],
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
    response: BitVec<u8, Lsb0>,
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
    fn shift_bit(&mut self, tms: bool, tdi: bool, capture_tdo: bool) -> bool {
        let seq = self.current_sequence.get_or_insert_with(|| JtagSequence {
            tck_cycles: 0,
            tdo_capture: capture_tdo,
            tms,
            data: Vec::with_capacity(64),
        });

        if let Some(complete_sequence) = seq.append(tdi, tms, capture_tdo) {
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
