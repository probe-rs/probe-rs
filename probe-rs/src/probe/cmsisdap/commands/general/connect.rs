use super::super::{Category, Request, Response, SendError};

#[derive(Clone, Copy, Debug)]
pub enum ConnectRequest {
    UseDefaultPort = 0x00,
    UseSWD = 0x01,
    UseJTAG = 0x02,
}

impl Request for ConnectRequest {
    const CATEGORY: Category = Category(0x02);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize, SendError> {
        buffer[offset] = *self as u8;
        Ok(1)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ConnectResponse {
    InitFailed = 0x00,
    SuccessfulInitForSWD = 0x01,
    SuccessfulInitForJTAG = 0x02,
}

impl Response for ConnectResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self, SendError> {
        match buffer[offset] {
            0 => Ok(ConnectResponse::InitFailed),
            1 => Ok(ConnectResponse::SuccessfulInitForSWD),
            2 => Ok(ConnectResponse::SuccessfulInitForJTAG),
            _ => Err(SendError::ConnectResponseError(buffer[offset])),
        }
    }
}
