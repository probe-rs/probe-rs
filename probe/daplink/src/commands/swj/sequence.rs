/// Implementation of the DAP_SWJ_SEQUENCE command
/// 

use crate::commands::{
    Response,
    Category,
    Request,
    Error,
    Result,
    Status,
};

#[derive(Clone, Copy)]
pub struct SequenceRequest {
    bit_count: u8,
    data: [u8;32],
}

impl Request for SequenceRequest {
    const CATEGORY: Category = Category(0x12);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        buffer[offset] = self.bit_count;

        // calculate transfer len in bytes
        let mut transfer_len_bytes: usize = (self.bit_count / 8) as usize;

        // A bit_count of zero means that we want to transmit 256 bits
        if self.bit_count == 0 {
            transfer_len_bytes = 256 / 8;
        }

        if self.bit_count % 8 != 0 {
            transfer_len_bytes += 1;
        }
        
        buffer[(offset+1)..(offset+1+transfer_len_bytes)].copy_from_slice(&self.data[..transfer_len_bytes]);

        // bit_count + data
        Ok(1 + transfer_len_bytes)
    }
}

impl SequenceRequest {
    pub(crate) fn new(data: &[u8]) -> Result<SequenceRequest> {
        if data.len() > 32 {
            return Err(Error::TooMuchData);
        }

        let bit_count = match data.len() {
            32 => 0,
            x => x*8,
        } as u8;

        let mut owned_data = [0u8;32];

        owned_data[..data.len()].copy_from_slice(data);

        Ok(SequenceRequest {
            bit_count,
            data: owned_data,
        })
    }
}

pub struct StructResponse(pub(crate) Status);

impl Response for StructResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        Ok(StructResponse(Status::from_byte(buffer[offset])?))
    }
}
