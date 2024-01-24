/// Implementation of the DAP_JTAG_SEQUENCE command
use super::super::{CmsisDapError, CommandId, Request, SendError, Status};

#[derive(Clone, Copy, Debug)]
pub struct Sequence {
    /// Number of TCK cycles: 1..64 (64 encoded as 0)
    cycles: u8,

    /// Do we drive the SWD pins, or do we capture
    is_output: bool,

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
    pub(crate) fn new(cycles: u8, is_output: bool, swdio: [u8; 8]) -> Result<Self, CmsisDapError> {
        assert!(
            cycles > 0 && cycles <= 64,
            "cycles = {}, but expected [1,64]",
            cycles
        );

        Ok(Self {
            cycles,
            is_output,
            data: swdio,
        })
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
    const COMMAND_ID: CommandId = CommandId::SwdSequence;

    type Response = SequenceResponse;

    /*
    | BYTE | BYTE **********| BYTE *********| BYTE ******|
    > 0x1D | Sequence Count | Sequence Info | SWDIO Data |
    |******|****************|///////////////|++++++++++++|
     */
    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        let mut transfer_len_bytes = 0;
        buffer[transfer_len_bytes] = self.sequences.len() as u8;
        transfer_len_bytes += 1;

        self.sequences.iter().for_each(|&sequence| {
            let swclk_cycles = sequence.cycles & 0x3F;
            let swclk_cycles = if swclk_cycles == 0 { 64 } else { swclk_cycles };

            let mut sequence_info = 0;
            sequence_info |= swclk_cycles;
            sequence_info |= (!sequence.is_output as u8) << 7;
            buffer[transfer_len_bytes] = sequence_info;
            transfer_len_bytes += 1;

            let byte_count: usize = (swclk_cycles as usize + 7) / 8;
            buffer[transfer_len_bytes..(transfer_len_bytes + byte_count)]
                .copy_from_slice(&sequence.data[..byte_count]);
            transfer_len_bytes += byte_count;
        });
        Ok(transfer_len_bytes)
    }

    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        let mut received_len_bytes = 1;
        let status = Status::from_byte(buffer[0])?;

        /*
        if status != Status::DAPOk {
            return Ok(SequenceResponse(status, vec![]));
        }
        */

        self.sequences.iter().for_each(|&sequence| {
            if !sequence.is_output {
                let swclk = sequence.cycles & 0x3F;
                let swclk_cycles = if swclk == 0 { 64 } else { swclk };
                let byte_count: usize = (swclk_cycles as usize + 7) / 8;
                received_len_bytes += byte_count;
            }
        });

        tracing::trace!("Expecting {} bytes in response", received_len_bytes - 1);

        let response = buffer[1..received_len_bytes].to_vec();
        Ok(SequenceResponse(status, response))
    }
}

#[derive(Debug)]
pub struct SequenceResponse(pub(crate) Status, pub(crate) Vec<u8>);
