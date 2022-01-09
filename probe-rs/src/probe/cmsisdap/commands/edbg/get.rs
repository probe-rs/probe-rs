use super::super::{CommandId, Request, SendError};

pub struct EdbgGetRequest<'a> {
    pub fragment_info: u8,
    pub command_packet: &'a [u8],
}

impl Request for EdbgGetRequest<'_> {
    const COMMAND_ID: CommandId = CommandId::EdbgGet;

    type Response = EdbgGetResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        unimplemented!();
        buffer[0] = self.fragment_info;
        let len = self.command_packet.len() as u16;
        buffer[1..3].copy_from_slice(&len.to_le_bytes());

        Ok(len as usize + 3)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        unimplemented!();
        let done = buffer[1] == 0x01;
        Ok(EdbgGetResponse { done })
    }
}

pub struct EdbgGetResponse {
    done: bool,
}
