use super::super::{Category, Request, Response, Result};


pub struct EdbgSetRequest<'a> {
    pub fragment_info: u8,
    pub command_packet: &'a [u8],
}

impl Request for EdbgSetRequest<'_> {
    const CATEGORY: Category = Category(0x89);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        unimplemented!();
        buffer[offset] = self.fragment_info;
        let len = self.command_packet.len() as u16;
        buffer[(offset+1) .. (offset+3)].copy_from_slice(&len.to_le_bytes());


        Ok(len as usize + 3)
    }
}

pub struct EdbgSetResponse {
    done: bool,
}
impl Response for EdbgSetResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        unimplemented!();
        let done = buffer[offset+1] == 0x01;
        Ok(EdbgSetResponse{done})
    }
}

