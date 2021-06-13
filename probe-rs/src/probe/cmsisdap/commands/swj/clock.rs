use super::super::{Category, Request, Response, SendError, Status};

#[derive(Debug)]
pub struct SWJClockRequest(pub(crate) u32);

impl Request for SWJClockRequest {
    const CATEGORY: Category = Category(0x11);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize, SendError> {
        use scroll::{Pwrite, LE};

        buffer
            .pwrite_with(self.0, offset, LE)
            .map_err(|_| SendError::Bug)?;
        Ok(4)
    }
}

#[derive(Debug)]
pub(crate) struct SWJClockResponse(pub(crate) Status);

impl Response for SWJClockResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self, SendError> {
        Ok(SWJClockResponse(Status::from_byte(buffer[offset])?))
    }
}
