use super::super::{Category, Request, Response, Result};
use scroll::{Pread, BE};

pub struct AvrRSPRequest;

impl Request for AvrRSPRequest {
    const CATEGORY: Category = Category(0x81);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        Ok(0)
    }
}

pub struct AvrRSPResponse {
    pub fragment_info: u8,
    pub command_packet: Vec<u8>,
}
impl Response for AvrRSPResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        let fragment_info = buffer[offset];
        if fragment_info == 0 {
            Ok(AvrRSPResponse {
                fragment_info: buffer[offset],
                command_packet: vec![],
            })
        } else {
            let size: u16 = buffer
                .pread_with(offset + 1, BE)
                .expect("Failed to read size");
            Ok(AvrRSPResponse {
                fragment_info: buffer[offset],
                command_packet: buffer[offset + 3..offset + 3 + size as usize].to_vec(),
            })
        }
    }
}
