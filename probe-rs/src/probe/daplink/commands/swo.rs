use super::{Category, CmsisDapError, Request, Response, Result, Status};
use anyhow::anyhow;
use std::convert::TryInto;

#[repr(u8)]
#[allow(unused)]
#[derive(Copy, Clone, Debug)]
pub enum TransportRequest {
    NoTransport = 0,
    DataCommand = 1,
    WinUsbEndpoint = 2,
}

impl Request for TransportRequest {
    const CATEGORY: Category = Category(0x17);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        buffer[offset] = *self as u8;
        Ok(1)
    }
}

#[derive(Debug)]
pub struct TransportResponse(pub(crate) Status);

impl Response for TransportResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        Ok(TransportResponse(Status::from_byte(buffer[offset])?))
    }
}

#[repr(u8)]
#[allow(unused)]
#[derive(Copy, Clone, Debug)]
pub enum ModeRequest {
    Off = 0,
    Uart = 1,
    Manchester = 2,
}

impl Request for ModeRequest {
    const CATEGORY: Category = Category(0x18);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        buffer[offset] = *self as u8;
        Ok(1)
    }
}

#[derive(Debug)]
pub struct ModeResponse(pub(crate) Status);

impl Response for ModeResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        Ok(ModeResponse(Status::from_byte(buffer[offset])?))
    }
}

#[derive(Copy, Clone, Debug)]
pub struct BaudrateRequest(pub(crate) u32);

impl Request for BaudrateRequest {
    const CATEGORY: Category = Category(0x19);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        assert!(
            buffer.len() >= offset + 4,
            "This is a bug. Please report it."
        );
        buffer[offset..offset + 4].copy_from_slice(&self.0.to_le_bytes());
        Ok(4)
    }
}

#[derive(Copy, Clone, Debug)]
pub struct BaudrateResponse(pub(crate) u32);

impl Response for BaudrateResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        if buffer.len() - offset < 4 {
            return Err(anyhow!(CmsisDapError::NotEnoughData));
        }

        let baud = u32::from_le_bytes(
            buffer[offset..offset + 4]
                .try_into()
                .expect("This is a bug. Please report it."),
        );

        Ok(BaudrateResponse(baud))
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum ControlRequest {
    Stop = 0,
    Start = 1,
}

impl Request for ControlRequest {
    const CATEGORY: Category = Category(0x1a);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        buffer[offset] = *self as u8;
        Ok(1)
    }
}

#[derive(Debug)]
pub struct ControlResponse(pub(crate) Status);

impl Response for ControlResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        Ok(ControlResponse(Status::from_byte(buffer[offset])?))
    }
}

#[derive(Debug)]
pub struct StatusRequest;

impl Request for StatusRequest {
    const CATEGORY: Category = Category(0x1b);

    fn to_bytes(&self, _buffer: &mut [u8], _offset: usize) -> Result<usize> {
        Ok(0)
    }
}

#[derive(Copy, Clone, Debug)]
pub struct TraceStatus {
    pub(crate) active: bool,
    pub(crate) error: bool,
    pub(crate) overrun: bool,
}

impl From<u8> for TraceStatus {
    fn from(value: u8) -> Self {
        Self {
            active: value & (1 << 0) != 0,
            error: value & (1 << 6) != 0,
            overrun: value & (1 << 7) != 0,
        }
    }
}

#[derive(Debug)]
pub struct StatusResponse {
    pub(crate) status: TraceStatus,
    pub(crate) count: u32,
}

impl Response for StatusResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        let status = TraceStatus::from(buffer[offset]);
        let count = u32::from_le_bytes(
            buffer[offset + 1..offset + 5]
                .try_into()
                .expect("This is a bug. Please report it."),
        );
        Ok(StatusResponse { status, count })
    }
}

#[derive(Debug)]
pub struct ExtendedStatusRequest {
    pub(crate) request_status: bool,
    pub(crate) request_count: bool,
    pub(crate) request_index_timestamp: bool,
}

impl Request for ExtendedStatusRequest {
    const CATEGORY: Category = Category(0x1e);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        let control = (self.request_status as u8)
            | ((self.request_count as u8) << 1)
            | ((self.request_index_timestamp as u8) << 2);
        buffer[offset] = control;
        Ok(1)
    }
}

#[derive(Debug)]
pub struct ExtendedStatusResponse {
    pub(crate) status: TraceStatus,
    pub(crate) count: u32,
    pub(crate) index: u32,
    pub(crate) timestamp: u32,
}

impl Response for ExtendedStatusResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        if buffer.len() - offset < 13 {
            return Err(anyhow!(CmsisDapError::NotEnoughData));
        }

        let status = TraceStatus::from(buffer[offset]);
        let count = u32::from_le_bytes(
            buffer[offset + 1..offset + 5]
                .try_into()
                .expect("This is a bug. Please report it."),
        );
        let index = u32::from_le_bytes(
            buffer[offset + 5..offset + 9]
                .try_into()
                .expect("This is a bug. Please report it."),
        );
        let timestamp = u32::from_le_bytes(
            buffer[offset + 9..offset + 13]
                .try_into()
                .expect("This is a bug. Please report it."),
        );
        Ok(ExtendedStatusResponse {
            status,
            count,
            index,
            timestamp,
        })
    }
}

#[derive(Debug)]
pub struct DataRequest {
    pub(crate) max_count: u16,
}

impl Request for DataRequest {
    const CATEGORY: Category = Category(0x1c);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        assert!(
            buffer.len() >= offset + 2,
            "This is a bug. Please report it."
        );
        buffer[offset..offset + 2].copy_from_slice(&self.max_count.to_le_bytes());
        Ok(2)
    }
}

#[derive(Debug)]
pub struct DataResponse {
    pub(crate) status: TraceStatus,
    pub(crate) data: Vec<u8>,
}

impl Response for DataResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        let status = TraceStatus::from(buffer[offset]);
        let count = u16::from_le_bytes(
            buffer[offset + 1..offset + 3]
                .try_into()
                .expect("This is a bug. Please report it."),
        );
        let start = offset + 3;
        let end = start + count as usize;
        if end > buffer.len() {
            return Err(anyhow!(CmsisDapError::NotEnoughData));
        }

        Ok(DataResponse {
            status,
            data: buffer[start..end].to_vec(),
        })
    }
}
