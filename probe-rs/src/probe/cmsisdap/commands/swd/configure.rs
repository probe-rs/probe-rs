use super::super::{Category, Request, SendError, Status};

#[derive(Debug)]
pub struct ConfigureRequest;

impl Request for ConfigureRequest {
    const CATEGORY: Category = Category(0x13);

    type Response = ConfigureResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        // TODO: Allow configuration
        buffer[0] = 0;
        Ok(1)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        Ok(ConfigureResponse(Status::from_byte(buffer[0])?))
    }
}

#[derive(Debug)]
pub struct ConfigureResponse(pub(crate) Status);
