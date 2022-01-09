use super::super::{CommandId, Request, SendError};
use scroll::{Pread, LE};

pub struct AvrEventRequest;

impl Request for AvrEventRequest {
    const COMMAND_ID: CommandId = CommandId::AvrEvent;

    type Response = AvrEventResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        Ok(0)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        let size: u16 = buffer.pread_with(0, LE).expect("Failed to read size");
        Ok(AvrEventResponse {
            events: buffer[2..2 + size as usize].to_vec(),
        })
    }
}

pub struct AvrEventResponse {
    pub events: Vec<u8>,
}
