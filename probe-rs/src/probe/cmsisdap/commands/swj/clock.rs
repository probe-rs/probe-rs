use super::super::{CommandId, Request, SendError, Status};

#[derive(Debug, Copy, Clone)]
pub struct SWJClockRequest {
    /// The clock speed of SWJ in Hz.
    pub(crate) clock_speed_hz: u32,
}

impl Request for SWJClockRequest {
    const COMMAND_ID: CommandId = CommandId::SwjClock;

    type Response = SWJClockResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        use scroll::{Pwrite, LE};

        buffer
            .pwrite_with(self.clock_speed_hz, 0, LE)
            .expect("Buffer for CMSIS-DAP command is too small. This is a bug, please report it.");
        Ok(4)
    }

    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        Ok(SWJClockResponse {
            status: Status::from_byte(buffer[0])?,
        })
    }
}

#[derive(Debug)]
pub(crate) struct SWJClockResponse {
    pub(crate) status: Status,
}
