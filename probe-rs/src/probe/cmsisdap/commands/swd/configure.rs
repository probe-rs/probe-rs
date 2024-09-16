use super::super::{CommandId, Request, SendError, Status};

#[derive(Debug, Copy, Clone)]
pub struct ConfigureRequest;

impl Request for ConfigureRequest {
    const COMMAND_ID: CommandId = CommandId::SwdConfigure;

    type Response = ConfigureResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        // TODO: Allow configuration
        buffer[0] = 0;
        Ok(1)
    }

    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        Ok(ConfigureResponse {
            status: Status::from_byte(buffer[0])?,
        })
    }
}

#[derive(Debug)]
pub struct ConfigureResponse {
    pub(crate) status: Status,
}
