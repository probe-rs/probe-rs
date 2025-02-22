//! WCH-LinkRV commands

use super::{DMI_OP_NOP, DMI_OP_READ, DMI_OP_WRITE, RiscvChip, WchLinkError, WchLinkVariant};

/// Only part of commands are implemented
#[repr(u8)]
pub enum CommandId {
    /// Probe control
    Control = 0x0D,
    /// Config chip, flash protection, etc
    ConfigChip = 0x01,
    /// Chip reset
    Reset = 0x0b,
    /// Set chip type and connection speed
    SetSpeed = 0x0c,
    /// DMI operations
    DmiOp = 0x08,
}

pub(crate) trait WchLinkCommand {
    const COMMAND_ID: CommandId;
    type Response: WchLinkCommandResponse;

    fn payload(&self) -> Vec<u8>;

    /// Convert the request to bytes, which can be sent to the probe.
    /// Returns the amount of bytes written to the buffer.
    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, super::WchLinkError> {
        let payload = self.payload();
        let payload_len = payload.len();

        buffer[0] = 0x81;
        buffer[1] = Self::COMMAND_ID as u8;
        buffer[2] = payload_len as u8;
        buffer[3..payload_len + 3].copy_from_slice(&payload);
        Ok(payload_len + 3)
    }

    /// Parse the response to this request from received bytes.
    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, super::WchLinkError> {
        Self::Response::from_raw(buffer)
    }
}

pub(crate) trait WchLinkCommandResponse {
    /// parse from the PAYLOAD part only
    fn from_payload(bytes: &[u8]) -> Result<Self, WchLinkError>
    where
        Self: Sized;

    /// default implementation for parsing [0x82 CMD LEN PAYLOAD] style response
    fn from_raw(resp: &[u8]) -> Result<Self, WchLinkError>
    where
        Self: Sized,
    {
        if resp[0] == 0x81 {
            let reason = resp[1];
            let len = resp[2] as usize;
            if len != resp[3..].len() {
                return Err(WchLinkError::InvalidPayload);
            }
            if reason == 0x55 {
                return Err(WchLinkError::Protocol(reason, resp.to_vec()));
            }
            Err(WchLinkError::Protocol(reason, resp.to_vec()))
        } else if resp[0] == 0x82 {
            let len = resp[2] as usize;
            if len != resp[3..].len() {
                return Err(WchLinkError::InvalidPayload);
            }
            let payload = resp[3..3 + len].to_vec();
            Self::from_payload(&payload)
        } else {
            Err(WchLinkError::InvalidPayload)
        }
    }
}

impl WchLinkCommandResponse for () {
    fn from_payload(_bytes: &[u8]) -> Result<Self, WchLinkError> {
        Ok(())
    }
}
impl WchLinkCommandResponse for u8 {
    fn from_payload(bytes: &[u8]) -> Result<Self, WchLinkError> {
        if bytes.len() != 1 {
            Err(WchLinkError::InvalidPayload)
        } else {
            Ok(bytes[0])
        }
    }
}

/// Get current probe info, version, etc
#[derive(Debug)]
pub struct GetProbeInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetProbeInfoResponse {
    pub major_version: u8,
    pub minor_version: u8,
    pub variant: WchLinkVariant,
}

impl WchLinkCommandResponse for GetProbeInfoResponse {
    fn from_payload(bytes: &[u8]) -> Result<Self, WchLinkError> {
        if bytes.len() < 3 {
            return Err(WchLinkError::InvalidPayload);
        }

        Ok(GetProbeInfoResponse {
            major_version: bytes[0],
            minor_version: bytes[1],
            variant: WchLinkVariant::try_from_u8(bytes[2])?,
        })
    }
}

impl WchLinkCommand for GetProbeInfo {
    const COMMAND_ID: CommandId = CommandId::Control;
    type Response = GetProbeInfoResponse;

    fn payload(&self) -> Vec<u8> {
        vec![0x01]
    }
}

/// Attach to the target chip
#[derive(Debug)]
pub struct AttachChip;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttachChipResponse {
    pub chip_family: RiscvChip,
    pub riscvchip: u8,
    pub chip_id: u32,
}

impl WchLinkCommandResponse for AttachChipResponse {
    fn from_payload(bytes: &[u8]) -> Result<Self, WchLinkError> {
        if bytes.len() != 5 {
            return Err(WchLinkError::InvalidPayload);
        }
        Ok(Self {
            chip_family: RiscvChip::try_from_u8(bytes[0])
                .ok_or(WchLinkError::UnknownChip(bytes[0]))?,
            riscvchip: bytes[0],
            chip_id: u32::from_be_bytes(bytes[1..5].try_into().unwrap()),
        })
    }
}

impl WchLinkCommand for AttachChip {
    const COMMAND_ID: CommandId = CommandId::Control;
    type Response = AttachChipResponse;

    fn payload(&self) -> Vec<u8> {
        vec![0x02]
    }
}

/// Detach from the target chip, aka. `OptEnd`
#[derive(Debug)]
pub struct DetachChip;

impl WchLinkCommand for DetachChip {
    const COMMAND_ID: CommandId = CommandId::Control;
    type Response = ();

    fn payload(&self) -> Vec<u8> {
        vec![0xff]
    }
}

/// Set speed
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
#[derive(Default)]
pub enum Speed {
    /// 400kHz
    Low = 0x03,
    /// 4000kHz
    Medium = 0x02,
    /// 6000kHz
    #[default]
    High = 0x01,
}

impl Speed {
    pub fn to_khz(self) -> u32 {
        match self {
            Speed::Low => 400,
            Speed::Medium => 4000,
            Speed::High => 6000,
        }
    }

    pub fn from_khz(khz: u32) -> Option<Self> {
        if khz >= 6000 {
            Some(Speed::High)
        } else if khz >= 4000 {
            Some(Speed::Medium)
        } else if khz >= 400 {
            Some(Speed::Low)
        } else {
            None
        }
    }
}

/// Set chip family and speed
#[derive(Debug)]
pub struct SetSpeed(pub super::RiscvChip, pub Speed);

impl WchLinkCommand for SetSpeed {
    const COMMAND_ID: CommandId = CommandId::SetSpeed;
    type Response = u8;

    fn payload(&self) -> Vec<u8> {
        vec![self.0 as u8, self.1 as u8]
    }
}

/// RISC-V DMI operations
#[derive(Debug)]
pub enum DmiOp {
    Nop,
    Read { addr: u8 },
    Write { addr: u8, data: u32 },
}

impl DmiOp {
    pub fn nop() -> Self {
        Self::Nop
    }
    pub fn read(addr: u8) -> Self {
        Self::Read { addr }
    }
    pub fn write(addr: u8, data: u32) -> Self {
        Self::Write { addr, data }
    }
}

#[derive(Debug)]
pub struct DmiOpResponse {
    pub addr: u8,
    pub data: u32,
    pub op: u8,
}

impl WchLinkCommandResponse for DmiOpResponse {
    fn from_payload(bytes: &[u8]) -> Result<Self, WchLinkError> {
        if bytes.len() != 6 {
            return Err(WchLinkError::InvalidPayload);
        }
        let addr = bytes[0];
        let op = bytes[5];
        let data = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
        Ok(DmiOpResponse { addr, data, op })
    }
}

impl WchLinkCommand for DmiOp {
    const COMMAND_ID: CommandId = CommandId::DmiOp;
    type Response = DmiOpResponse;

    fn payload(&self) -> Vec<u8> {
        let mut bytes = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        match self {
            DmiOp::Nop => {
                bytes[5] = DMI_OP_NOP;
            }
            DmiOp::Read { addr } => {
                bytes[0] = *addr;
                bytes[5] = DMI_OP_READ;
            }
            DmiOp::Write { addr, data } => {
                bytes[0] = *addr;
                bytes[5] = DMI_OP_WRITE;
                bytes[1..5].copy_from_slice(&data.to_be_bytes());
            }
        }
        bytes
    }
}

/// Reset the chip
#[derive(Debug)]
pub struct ResetTarget;

impl WchLinkCommand for ResetTarget {
    const COMMAND_ID: CommandId = CommandId::Reset;
    type Response = ();
    fn payload(&self) -> Vec<u8> {
        vec![0x01]
    }
}

/// Check flash protection status
#[derive(Debug)]
pub struct CheckFlashProtection;
impl WchLinkCommand for CheckFlashProtection {
    const COMMAND_ID: CommandId = CommandId::ConfigChip;
    type Response = u8;

    fn payload(&self) -> Vec<u8> {
        vec![0x01]
    }
}

/// Unprotect flash
#[derive(Debug)]
pub struct UnprotectFlash;
impl WchLinkCommand for UnprotectFlash {
    const COMMAND_ID: CommandId = CommandId::ConfigChip;
    type Response = ();

    fn payload(&self) -> Vec<u8> {
        vec![0x02]
    }
}
