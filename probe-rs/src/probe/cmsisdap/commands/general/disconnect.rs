use super::super::{Category, Request, Response, SendError, Status};

#[derive(Clone, Copy, Debug)]
pub struct DisconnectRequest;

impl Request for DisconnectRequest {
    const CATEGORY: Category = Category(0x03);

    fn to_bytes(&self, _buffer: &mut [u8], _offset: usize) -> Result<usize, SendError> {
        Ok(0)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DisconnectResponse(pub(crate) Status);

impl Response for DisconnectResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self, SendError> {
        Ok(DisconnectResponse(Status::from_byte(buffer[offset])?))
    }
}
