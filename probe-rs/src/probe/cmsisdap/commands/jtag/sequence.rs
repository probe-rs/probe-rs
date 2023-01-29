/// Implementation of the DAP_JTAG_SEQUENCE command
///
use super::super::{CmsisDapError, CommandId, Request, SendError, Status};

#[derive(Clone, Copy, Debug)]
pub enum SequenceData {
    TMS(u8),
    TDI(u8),
}

#[derive(Clone, Copy, Debug)]
pub struct Sequence {
    /// Number of TCK cycles: 1..64 (64 encoded as 0)
    tck_cycles: u8,

    /// TDO capture
    tdo_capture: bool,

    /// TMS value
    tms: bool,

    /// Data generated on TDI
    data: [u8; 8],
}

impl Sequence {
    pub(crate) fn new(
        tck_cycles: u8,
        tdo_capture: bool,
        tms: bool,
        data: [u8; 8],
    ) -> Result<Self, CmsisDapError> {
        if (tck_cycles > 64) {
            return Err(CmsisDapError::JTAGSequenceTooManyClockSequences);
        }
        Ok(Self {
            tck_cycles,
            tdo_capture,
            tms,
            data,
        })
    }
}

#[derive(Clone, Debug)]
pub struct SequenceRequest {
    sequences: Vec<Sequence>,
}

impl SequenceRequest {
    pub(crate) fn new(sequences: Vec<Sequence>) -> Result<Self, CmsisDapError> {
        if sequences.len() > (u8::MAX as usize) {
            return Err(CmsisDapError::JTAGSequenceTooMuchData);
        }
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
            sequence_info = sequence_info | (if tck_cycles == 64 { 0 } else { tck_cycles });
            sequence_info = sequence_info | ((sequence.tms as u8) << 6);
            sequence_info = sequence_info | ((sequence.tdo_capture as u8) << 7);
            buffer[transfer_len_bytes] = sequence_info;
            transfer_len_bytes += 1;

            let byte_count: usize = (tck_cycles as usize + 7) / 8;
            buffer[transfer_len_bytes..(transfer_len_bytes + byte_count)]
                .copy_from_slice(&sequence.data[..byte_count]);
            transfer_len_bytes += byte_count;
        });
        Ok(transfer_len_bytes)
    }

    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        let mut received_len_bytes = 1;
        let status = Status::from_byte(buffer[0])?;

        self.sequences.iter().for_each(|&sequence| {
            if sequence.tdo_capture {
                let tck_cycles = sequence.tck_cycles & 0x3F;
                let tck_cycles = if tck_cycles == 0 { 64 } else { tck_cycles };
                let byte_count: usize = (tck_cycles as usize + 7) / 8;
                received_len_bytes += byte_count;
            }
        });

        let response = buffer[1..received_len_bytes].to_vec();
        Ok(SequenceResponse(status, response))
    }
}

#[derive(Debug)]
pub struct SequenceResponse(pub(crate) Status, pub(crate) Vec<u8>);
