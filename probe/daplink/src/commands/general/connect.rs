use crate::commands::{
    Response,
    Category,
    Request,
    Error,
    Result,
};

#[derive(Clone, Copy)]
pub enum ConnectRequest {
    UseDefaultPort = 0x01,
    UseSWD = 0x02,
    UseJTAG = 0x03,
}

impl Request for ConnectRequest {
    const CATEGORY: Category = Category(0x02);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        buffer[offset] = *self as u8;
        Ok(1)
    }
}

pub enum ConnectResponse {
    InitFailed = 0x01,
    SuccessfulInitForSWD = 0x02,
    SuccessfulInitForJTAG = 0x03,
}

impl Response for ConnectResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        match buffer[offset] {
            0 => Ok(ConnectResponse::InitFailed),
            1 => Ok(ConnectResponse::SuccessfulInitForSWD),
            2 => Ok(ConnectResponse::SuccessfulInitForJTAG),
            _ => Err(Error::UnexpectedAnswer)
        }
    }
}