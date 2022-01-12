use super::super::{CommandId, Request, SendError};
use scroll::{Pread, BE};

pub struct AvrRSPRequest;

impl Request for AvrRSPRequest {
    const COMMAND_ID: CommandId = CommandId::AvrResponse;

    type Response = AvrRSPResponse;

    fn to_bytes(&self, _buffer: &mut [u8]) -> Result<usize, SendError> {
        Ok(0)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        let fragment_info = buffer[0];
        if fragment_info == 0 {
            Ok(AvrRSPResponse {
                fragment_info: buffer[0],
                command_packet: vec![],
            })
        } else {
            let size: u16 = buffer.pread_with(1, BE).expect("Failed to read size");
            Ok(AvrRSPResponse {
                fragment_info: buffer[0],
                command_packet: buffer[3..3 + size as usize].to_vec(),
            })
        }
    }
}

pub struct AvrRSPResponse {
    pub fragment_info: u8,
    pub command_packet: Vec<u8>,
}
