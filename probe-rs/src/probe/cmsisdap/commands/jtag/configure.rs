/// Implementation of the DAP_JTAG_CONFIGURE command
//
use super::super::{CmsisDapError, CommandId, Request, SendError, Status};

#[derive(Clone, Debug)]
pub struct ConfigureRequest {
    ir_lengths: Vec<u8>,
}

impl ConfigureRequest {
    pub(crate) fn new(ir_lengths: Vec<u8>) -> Result<ConfigureRequest, CmsisDapError> {
        if ir_lengths.len() > (u8::MAX as usize) {
            return Err(CmsisDapError::JTAGConfigureTooMuchData);
        }
        Ok(ConfigureRequest { ir_lengths })
    }
}

impl Request for ConfigureRequest {
    const COMMAND_ID: CommandId = CommandId::JtagConfigure;

    type Response = ConfigureResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = self.ir_lengths.len() as u8;
        buffer[1..self.ir_lengths.len() + 1].copy_from_slice(&self.ir_lengths[..]);
        Ok(self.ir_lengths.len() + 1)
    }

    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        let status = Status::from_byte(buffer[0])?;

        Ok(ConfigureResponse(status))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ConfigureResponse(pub(crate) Status);
