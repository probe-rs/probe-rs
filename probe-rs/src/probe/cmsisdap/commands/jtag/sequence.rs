/// Implementation of the DAP_JTAG_SEQUENCE command
use super::super::{CmsisDapError, CommandId, Request, SendError, Status};

use bitvec::prelude::*;

#[derive(Clone, Copy, Debug)]
pub struct Sequence {
    /// Number of TCK cycles: 1..64 (64 encoded as 0)
    tck_cycles: u8,

    /// TDO capture
    tdo_capture: bool,

    /// TMS value
    tms: bool,

    /// Data to generate on TDI
    data: [u8; 8],
}

impl Sequence {
    /// Create a JTAG sequence, optionally capturing TDO.
    ///
    /// # Args
    ///
    /// * `tck_cycles` - The number of cycles to clock out `data` bits on TDI
    /// * `tck_capture` - Whether the probe should capture TDO
    /// * `tms` - Whether TMS should be held high or low
    /// * `tdi` - The TDI bits to clock out
    pub(crate) fn new(
        tck_cycles: u8,
        tdo_capture: bool,
        tms: bool,
        tdi: [u8; 8],
    ) -> Result<Self, CmsisDapError> {
        assert!(
            tck_cycles > 0 && tck_cycles <= 64,
            "tck_cycles = {}, but expected [1,64]",
            tck_cycles
        );

        Ok(Self {
            tck_cycles,
            tdo_capture,
            tms,
            data: tdi,
        })
    }

    /// Create a JTAG sequence, capturing TDO.
    /// The number of TCK cycles is determined by the `tdi` len.
    ///
    /// # Args
    ///
    /// * `tms` - Whether TMS should be held high or low
    /// * `tdi` - The TDI bits to clock out
    pub(crate) fn capture(tms: bool, tdi: &BitSlice) -> Result<Self, CmsisDapError> {
        Self::new_from_bitslice(tms, tdi, true)
    }

    /// Create a JTAG sequence, *without* capturing TDO.
    /// The number of TCK cycles is determined by the `tdi` len.
    ///
    /// # Args
    ///
    /// * `tms` - Whether TMS should be held high or low
    /// * `tdi` - The TDI bits to clock out
    pub(crate) fn no_capture(tms: bool, tdi: &BitSlice) -> Result<Self, CmsisDapError> {
        Self::new_from_bitslice(tms, tdi, false)
    }

    fn new_from_bitslice(
        tms: bool,
        tdi: &BitSlice,
        tdo_capture: bool,
    ) -> Result<Self, CmsisDapError> {
        let tck_cycles = tdi.len();

        Self::new(
            tck_cycles as u8,
            tdo_capture,
            tms,
            tdi.load_le::<u64>().to_le_bytes(),
        )
    }
}

#[derive(Clone, Debug)]
pub struct SequenceRequest {
    sequences: Vec<Sequence>,
}

impl SequenceRequest {
    pub(crate) fn new(sequences: Vec<Sequence>) -> Result<Self, CmsisDapError> {
        assert!(
            !sequences.is_empty() && sequences.len() <= (u8::MAX as usize),
            "sequences.len() == {}, but expected [1,255]",
            sequences.len()
        );
        Ok(SequenceRequest { sequences })
    }
}

impl Request for SequenceRequest {
    const COMMAND_ID: CommandId = CommandId::JtagSequence;

    type Response = SequenceResponse;

    /*
    | BYTE | BYTE **********| BYTE *********| BYTE ****|
    > 0x14 | Sequence Count | Sequence Info | TDI Data |
    |******|****************|///////////////|//////////|
     */
    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        let mut transfer_len_bytes = 0;
        buffer[transfer_len_bytes] = self.sequences.len() as u8;
        transfer_len_bytes += 1;

        self.sequences.iter().for_each(|&sequence| {
            let tck_cycles = sequence.tck_cycles & 0x3F;
            let tck_cycles = if tck_cycles == 0 { 64 } else { tck_cycles };

            let mut sequence_info = 0;
            sequence_info |= if tck_cycles == 64 { 0 } else { tck_cycles };
            sequence_info |= (sequence.tms as u8) << 6;
            sequence_info |= (sequence.tdo_capture as u8) << 7;
            buffer[transfer_len_bytes] = sequence_info;
            transfer_len_bytes += 1;

            let byte_count = tck_cycles.div_ceil(8) as usize;
            buffer[transfer_len_bytes..][..byte_count]
                .copy_from_slice(&sequence.data[..byte_count]);
            transfer_len_bytes += byte_count;
        });
        Ok(transfer_len_bytes)
    }

    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        let mut received_len_bytes = 1;
        let status = Status::from_byte(buffer[0])?;

        let mut bits = BitVec::new();
        self.sequences
            .iter()
            .filter(|sequence| sequence.tdo_capture)
            .for_each(|&sequence| {
                let tck_cycles = sequence.tck_cycles as usize & 0x3F;
                let tck_cycles = if tck_cycles == 0 { 64 } else { tck_cycles };
                let byte_count = tck_cycles.div_ceil(8);
                let bytes = &buffer[received_len_bytes..][..byte_count];
                bits.extend_from_bitslice(&bytes.view_bits::<Lsb0>()[..tck_cycles]);
                received_len_bytes += byte_count;
            });

        Ok(SequenceResponse(status, bits))
    }
}

#[derive(Debug)]
pub struct SequenceResponse(pub(crate) Status, pub(crate) BitVec);
