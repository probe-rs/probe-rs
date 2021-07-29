use scroll::{Pread, LE};

use super::{CommandId, Request, SendError, Status};
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
    const COMMAND_ID: CommandId = CommandId::SwoTransport;

    type Response = TransportResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = *self as u8;
        Ok(1)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        Ok(TransportResponse(Status::from_byte(buffer[0])?))
    }
}

#[derive(Debug)]
pub struct TransportResponse(pub(crate) Status);

#[repr(u8)]
#[allow(unused)]
#[derive(Copy, Clone, Debug)]
pub enum ModeRequest {
    Off = 0,
    Uart = 1,
    Manchester = 2,
}

impl Request for ModeRequest {
    const COMMAND_ID: CommandId = CommandId::SwoMode;

    type Response = ModeResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = *self as u8;
        Ok(1)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        Ok(ModeResponse(Status::from_byte(buffer[0])?))
    }
}

#[derive(Debug)]
pub struct ModeResponse(pub(crate) Status);

#[derive(Copy, Clone, Debug)]
pub struct BaudrateRequest(pub(crate) u32);

impl Request for BaudrateRequest {
    const COMMAND_ID: CommandId = CommandId::SwoBaudrate;

    type Response = u32;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        assert!(
            buffer.len() >= 4,
            "Buffer for CMSIS-DAP command is too small. This is a bug, please report it."
        );
        buffer[0..4].copy_from_slice(&self.0.to_le_bytes());
        Ok(4)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        if buffer.len() < 4 {
            return Err(SendError::NotEnoughData);
        }

        let baud: u32 = buffer
            .pread_with(0, LE)
            .map_err(|_| SendError::NotEnoughData)?;

        Ok(baud)
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum ControlRequest {
    Stop = 0,
    Start = 1,
}

impl Request for ControlRequest {
    const COMMAND_ID: CommandId = CommandId::SwoControl;

    type Response = ControlResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = *self as u8;
        Ok(1)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        Ok(ControlResponse(Status::from_byte(buffer[0])?))
    }
}

#[derive(Debug)]
pub struct ControlResponse(pub(crate) Status);

#[derive(Debug)]
pub struct StatusRequest;

impl Request for StatusRequest {
    const COMMAND_ID: CommandId = CommandId::SwoStatus;

    type Response = StatusResponse;

    fn to_bytes(&self, _buffer: &mut [u8]) -> Result<usize, SendError> {
        Ok(0)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        let status = TraceStatus::from(buffer[0]);
        let count = u32::from_le_bytes(
            buffer[1..5]
                .try_into()
                .map_err(|_| SendError::NotEnoughData)?,
        );
        Ok(StatusResponse { status, count })
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

#[derive(Debug)]
pub struct ExtendedStatusRequest {
    pub(crate) request_status: bool,
    pub(crate) request_count: bool,
    pub(crate) request_index_timestamp: bool,
}

impl Request for ExtendedStatusRequest {
    const COMMAND_ID: CommandId = CommandId::SwoExtendedStatus;

    type Response = ExtendedStatusResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        let control = (self.request_status as u8)
            | ((self.request_count as u8) << 1)
            | ((self.request_index_timestamp as u8) << 2);
        buffer[0] = control;
        Ok(1)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        if buffer.len() < 13 {
            return Err(SendError::NotEnoughData);
        }

        let status = TraceStatus::from(buffer[0]);
        let count = u32::from_le_bytes(
            buffer[1..5]
                .try_into()
                .map_err(|_| SendError::NotEnoughData)?,
        );
        let index = u32::from_le_bytes(
            buffer[5..9]
                .try_into()
                .map_err(|_| SendError::NotEnoughData)?,
        );
        let timestamp = u32::from_le_bytes(
            buffer[9..13]
                .try_into()
                .map_err(|_| SendError::NotEnoughData)?,
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
pub struct ExtendedStatusResponse {
    pub(crate) status: TraceStatus,
    pub(crate) count: u32,
    pub(crate) index: u32,
    pub(crate) timestamp: u32,
}

#[derive(Debug)]
pub struct DataRequest {
    pub(crate) max_count: u16,
}

impl Request for DataRequest {
    const COMMAND_ID: CommandId = CommandId::SwoData;

    type Response = DataResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        assert!(
            buffer.len() >= 2,
            "Buffer for CMSIS-DAP command is too small. This is a bug, please report it."
        );
        buffer[0..2].copy_from_slice(&self.max_count.to_le_bytes());
        Ok(2)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        let status = TraceStatus::from(buffer[0]);
        let count = u16::from_le_bytes(
            buffer[1..3]
                .try_into()
                .map_err(|_| SendError::NotEnoughData)?,
        );
        let start = 3;
        let end = start + count as usize;
        if end > buffer.len() {
            return Err(SendError::NotEnoughData);
        }

        Ok(DataResponse {
            status,
            data: buffer[start..end].to_vec(),
        })
    }
}

#[derive(Debug)]
pub struct DataResponse {
    pub(crate) status: TraceStatus,
    pub(crate) data: Vec<u8>,
}
