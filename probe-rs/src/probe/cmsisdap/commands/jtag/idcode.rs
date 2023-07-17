/// Implementation of the DAP_JTAG_IDCODE command
use super::super::{CommandId, Request, SendError, Status};
#[derive(Clone, Copy, Debug)]
pub struct IdCodeRequest {
    index: u8,
}

impl IdCodeRequest {
    #[allow(dead_code)]
    pub(crate) fn new(index: u8) -> IdCodeRequest {
        IdCodeRequest { index }
    }
}

impl Request for IdCodeRequest {
    const COMMAND_ID: CommandId = CommandId::JtagIdcode;

    type Response = IDCODEResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = self.index;
        Ok(1)
    }

    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        if buffer.len() < 5 {
            return Err(SendError::NotEnoughData);
        }
        let status = Status::from_byte(buffer[0])?;

        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&buffer[1..=4]);

        let idcode: u32 = u32::from_le_bytes(bytes);

        let response = IDCODEResponse { status, idcode };

        Ok(response)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct IDCODEResponse {
    #[allow(dead_code)]
    pub(crate) status: Status,
    #[allow(dead_code)]
    pub(crate) idcode: u32,
}
