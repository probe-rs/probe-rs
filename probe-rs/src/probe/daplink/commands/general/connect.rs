use super::super::{Category, CmsisDapError, Request, Response, Result};
use anyhow::{anyhow, Context};

#[derive(Clone, Copy, Debug)]
pub enum ConnectRequest {
    UseDefaultPort = 0x00,
    UseSWD = 0x01,
    UseJTAG = 0x02,
}

impl Request for ConnectRequest {
    const CATEGORY: Category = Category(0x02);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
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
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        match buffer[offset] {
            0 => Ok(ConnectResponse::InitFailed),
            1 => Ok(ConnectResponse::SuccessfulInitForSWD),
            2 => Ok(ConnectResponse::SuccessfulInitForJTAG),
            _ => Err(anyhow!(CmsisDapError::UnexpectedAnswer)).with_context(|| {
                format!(
                    "Connecting to target failed, received: {:x}",
                    buffer[offset]
                )
            }),
        }
    }
}
