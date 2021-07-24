use super::super::{CommandId, Request, SendError};

#[derive(Clone, Copy, Debug)]
pub struct HostStatusRequest {
    status_type: u8,
    status: u8,
}

impl HostStatusRequest {
    pub fn connected(connected: bool) -> Self {
        HostStatusRequest {
            status_type: 0,
            status: connected as u8,
        }
    }

    #[allow(dead_code)]
    pub fn running(running: bool) -> Self {
        HostStatusRequest {
            status_type: 1,
            status: running as u8,
        }
    }
}

impl Request for HostStatusRequest {
    const COMMAND_ID: CommandId = CommandId::HostStatus;

    type Response = HostStatusResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = self.status_type;
        buffer[1] = self.status;
        Ok(2)
    }

    fn from_bytes(&self, _buffer: &[u8]) -> Result<Self::Response, SendError> {
        Ok(HostStatusResponse)
    }
}

#[derive(Copy, Clone, Debug)]
pub struct HostStatusResponse;
