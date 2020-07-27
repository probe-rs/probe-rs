use super::super::{Category, Request, Response, Result};

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
    const CATEGORY: Category = Category(0x01);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        buffer[offset] = self.status_type;
        buffer[offset + 1] = self.status;
        Ok(2)
    }
}

#[derive(Copy, Clone, Debug)]
pub struct HostStatusResponse;

impl Response for HostStatusResponse {
    fn from_bytes(_buffer: &[u8], _offset: usize) -> Result<Self> {
        Ok(HostStatusResponse)
    }
}
