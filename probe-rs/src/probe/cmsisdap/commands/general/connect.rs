use super::super::{CommandId, Request, SendError};

#[derive(Clone, Copy, Debug)]
pub enum ConnectRequest {
    UseDefaultPort = 0x00,
    UseSWD = 0x01,
    UseJTAG = 0x02,
}

impl Request for ConnectRequest {
    const COMMAND_ID: CommandId = CommandId::Connect;

    type Response = ConnectResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = *self as u8;
        Ok(1)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        match buffer[0] {
            0 => Ok(ConnectResponse::InitFailed),
            1 => Ok(ConnectResponse::SuccessfulInitForSWD),
            2 => Ok(ConnectResponse::SuccessfulInitForJTAG),
            other => Err(SendError::ConnectResponseError(other)),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ConnectResponse {
    InitFailed = 0x00,
    SuccessfulInitForSWD = 0x01,
    SuccessfulInitForJTAG = 0x02,
}
