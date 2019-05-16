use crate::commands::{
    Response,
    Category,
    Request,
    Result,
    Status,
};

#[derive(Debug)]
pub struct SWJClockRequest(pub(crate) u32);

impl Request for SWJClockRequest  {
    const CATEGORY: Category = Category(0x11);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        use scroll::Pwrite;

        buffer.pwrite(self.0, offset).expect("This is a bug. Please report it.");
        Ok(4)
    }
}

#[derive(Debug)]
pub(crate) struct SWJClockResponse(pub(crate) Status);

impl Response for SWJClockResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        Ok(SWJClockResponse(Status::from_byte(buffer[offset])?))
    }
}