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
    tck_cycles: u8,
    tdo_capture: bool,
    data: SequenceData,
}

impl Sequence {
    fn new(tck_cycles: u8, tdo_capture: bool, data: SequenceData) -> Result<Self, CmsisDapError> {
        if (tck_cycles > 64) {
            return Err(CmsisDapError::JTAGSequenceTooManyClockSequences);
        }
        Ok(Self {
            tck_cycles,
            tdo_capture,
            data,
        })
    }
}

#[derive(Clone, Debug)]
pub struct SequenceRequest {
    sequences: Vec<Sequence>,
}

impl SequenceRequest {
    fn new(sequences: Vec<Sequence>) -> Result<Self, CmsisDapError> {
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
        use SequenceData::*;
        let mut byte_count = 0;
        //Sequence Count
        buffer[byte_count] = self.sequences.len() as u8;

        byte_count += 1;
        self.sequences.iter().for_each(|&sequence| {
            let mut sequence_info = 0;
            sequence_info = sequence_info | (sequence.tck_cycles & 0x1F);
            sequence_info = sequence_info | ((if sequence.tdo_capture { 1u8 } else { 0u8 }) << 7);

            let sequence_data = match sequence.data {
                TMS(v) => {
                    sequence_info = sequence_info | (1u8 << 6);
                    v
                }
                TDI(v) => v,
            };

            buffer[byte_count] = sequence_info;
            byte_count += 1;
            buffer[byte_count] = sequence_data;
            byte_count += 1;
        });
        Ok(byte_count + 1)
    }

    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        let tdo_count = self
            .sequences
            .iter()
            .filter(|sequence| sequence.tdo_capture)
            .count();

        if buffer.len() < tdo_count + 1 {
            return Err(SendError::NotEnoughData);
        }
        let status = Status::from_byte(buffer[0])?;

        let tdo_data: Vec<u8> = buffer[1..=tdo_count]
            .iter()
            .map(|byte| byte.to_owned())
            .collect();

        Ok(SequenceResponse { status, tdo_data })
    }
}

pub struct SequenceResponse {
    pub(crate) status: Status,
    pub(crate) tdo_data: Vec<u8>,
}
