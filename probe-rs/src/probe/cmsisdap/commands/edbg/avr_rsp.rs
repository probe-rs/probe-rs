use super::super::{Category, Request, Response, Result};
use scroll::{Pread, LE};


pub struct AvrRSPRequest;

impl Request for AvrRSPRequest {
    const CATEGORY: Category = Category(0x81);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        Ok(0)
    }
}

pub struct AvrRSPResponse {
    pub fragment_info: u8,
    pub size: u16,
    pub command_packet: Vec<u8>,
}
impl Response for AvrRSPResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        let fragment_info = buffer[offset];
        if fragment_info == 0 {
            Ok(AvrRSPResponse{
                fragment_info: buffer[offset],
                size: 0,
                command_packet: vec![]
            })
        }
        else {
            Ok(AvrRSPResponse{
                fragment_info: buffer[offset],
                size: buffer.pread_with(offset+1, LE)
                    .expect("Failed to read size"),
                command_packet: buffer[offset+3..].to_vec(),
            })
        }
    }
}
