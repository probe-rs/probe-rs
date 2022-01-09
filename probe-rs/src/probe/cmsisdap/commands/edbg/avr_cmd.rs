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
        //buffer[(offset+1) .. (offset+3)].copy_from_slice(&len.to_be_bytes());
        buffer
            .pwrite_with(len, 1, BE)
            .expect("This is a bug. Please report it.");
        buffer[3..3 + len as usize].copy_from_slice(self.command_packet);

        Ok(len as usize + 3)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        let done = buffer[1] == 0x01;
        Ok(AvrCommandResponse { done })
    }
}

pub struct AvrCommandResponse {
    done: bool,
}
