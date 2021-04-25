use super::super::{Category, Request, Response, Result};
use scroll::{Pread, LE};


pub struct AvrEventRequest;

impl Request for AvrEventRequest {
    const CATEGORY: Category = Category(0x82);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        Ok(0)
    }
}

pub struct AvrEventResponse {
    pub events: Vec<u8>,
}
impl Response for AvrEventResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        let size: u16 = buffer.pread_with(offset, LE)
                .expect("Failed to read size");
        Ok(AvrEventResponse{
            events: buffer[offset+2..offset+ 2+ size as usize].to_vec(),
        })
    }
}
