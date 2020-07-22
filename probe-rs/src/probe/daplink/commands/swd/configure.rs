use super::super::{Category, Request, Response, Result, Status};

#[derive(Debug)]
pub struct ConfigureRequest;

impl Request for ConfigureRequest {
    const CATEGORY: Category = Category(0x13);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        // TODO: Allow configuration
        buffer[offset] = 0;
        Ok(1)
    }
}

#[derive(Debug)]
pub struct ConfigureResponse(pub(crate) Status);

impl Response for ConfigureResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        Ok(ConfigureResponse(Status::from_byte(buffer[offset])?))
    }
}
