use super::super::{CommandId, Request, SendError};
use scroll::{Pwrite, BE};

pub struct AvrCommand<'a> {
    pub fragment_info: u8,
    pub command_packet: &'a [u8],
}

impl Request for AvrCommand<'_> {
    const COMMAND_ID: CommandId = CommandId::AvrCmd;

    type Response = AvrCommandResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = self.fragment_info;
        let len = self.command_packet.len() as u16;
        buffer
            .pwrite_with(len, 1, BE)
            .expect("This is a bug. Please report it.");
        buffer[3..3 + len as usize].copy_from_slice(self.command_packet);

        Ok(len as usize + 3)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        let done = buffer[0] == 0x01;
        Ok(AvrCommandResponse { done })
    }
}

#[derive(Debug)]
pub struct AvrCommandResponse {
    pub done: bool,
}
