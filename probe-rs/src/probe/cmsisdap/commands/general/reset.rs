use super::super::{Category, Request, SendError, Status};

#[derive(Debug)]
pub struct ResetRequest;

impl Request for ResetRequest {
    const CATEGORY: Category = Category(0x0A);

    type Response = ResetResponse;

    fn to_bytes(&self, _buffer: &mut [u8], _offset: usize) -> Result<usize, SendError> {
        Ok(0)
    }

    fn from_bytes(&self, buffer: &[u8], offset: usize) -> Result<Self::Response, SendError> {
        Ok(ResetResponse {
            status: Status::from_byte(buffer[offset])?,
            execute: Execute::from_byte(buffer[offset + 1])?,
        })
    }
}

/// Execute: indicates whether a device specific reset sequence was executed.
#[derive(Debug)]
pub enum Execute {
    NoDeviceSpecificResetSequenceImplemented = 0,
    DeviceSpecificResetSequenceImplemented = 1,
}

impl Execute {
    pub(crate) fn from_byte(byte: u8) -> Result<Self, SendError> {
        match byte {
            0 => Ok(Execute::NoDeviceSpecificResetSequenceImplemented),
            1 => Ok(Execute::DeviceSpecificResetSequenceImplemented),
            _ => Err(SendError::UnexpectedAnswer),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ResetResponse {
    pub status: Status,
    pub execute: Execute,
}
