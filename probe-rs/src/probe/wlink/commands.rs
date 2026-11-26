//! WCH-LinkRV commands

use super::{DMI_OP_NOP, DMI_OP_READ, DMI_OP_WRITE, RiscvChip, WchLinkError, WchLinkVariant};

/// Only part of commands are implemented
#[repr(u8)]
pub enum CommandId {
    /// Probe control
    Control = 0x0D,
    /// Config chip, flash protection, write memory region.
    ConfigChip = 0x01,
    /// Flash operations.
    Program = 0x02,
    /// Extended flash protection queries (read/write protection)
    ChipProtect = 0x06,
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
        if buffer.len() < 3 || (buffer[0] == 0x82 && buffer[1] != Self::COMMAND_ID as u8) {
            return Err(WchLinkError::InvalidPayload);
        }
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
        if resp.len() < 3 {
            return Err(WchLinkError::InvalidPayload);
        }
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

/// Drive the RST pin of the probe. Only the WCH-LinkE and WCH-LinkW have this pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetRstPin {
    /// Drive the pin low, which resets the target.
    Low,
    /// Release the pin. The pull-up of the target pulls it high.
    Floating,
}

impl WchLinkCommand for SetRstPin {
    const COMMAND_ID: CommandId = CommandId::Control;
    type Response = ();

    fn payload(&self) -> Vec<u8> {
        let level = match self {
            SetRstPin::Low => 0x13,
            SetRstPin::Floating => 0x15,
        };
        vec![0x0e, level]
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

/// Set the memory region for a following flash write (CMD 0x01).
#[derive(Debug)]
pub struct SetWriteMemoryRegion {
    /// Absolute target address the firmware is written to.
    pub start_addr: u32,
    /// Length of the firmware image in bytes.
    pub len: u32,
}

impl WchLinkCommand for SetWriteMemoryRegion {
    const COMMAND_ID: CommandId = CommandId::ConfigChip;
    type Response = ();

    fn payload(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(8);
        bytes.extend_from_slice(&self.start_addr.to_be_bytes());
        bytes.extend_from_slice(&self.len.to_be_bytes());
        bytes
    }
}

/// Flash operations (CMD 0x02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program {
    /// Erase the whole code flash.
    EraseFlash = 0x01,
    /// Program the announced memory region.
    WriteFlash = 0x02,
    /// Upload the flash loader.
    WriteFlashOP = 0x05,
    /// Commit the uploaded flash loader. The probe answers `0x07` on success.
    Unknown07AfterFlashOPWritten = 0x07,
    /// Finish programming.
    End = 0x08,
}

impl WchLinkCommand for Program {
    const COMMAND_ID: CommandId = CommandId::Program;
    type Response = u8;

    fn payload(&self) -> Vec<u8> {
        vec![*self as u8]
    }
}

/// Query the flash read-protection status (CMD 0x06, subcommand 0x01).
///
/// Answers `0x01` when protected, `0x02` when unprotected.
#[derive(Debug)]
pub struct CheckReadProtect;

impl WchLinkCommand for CheckReadProtect {
    const COMMAND_ID: CommandId = CommandId::ChipProtect;
    type Response = u8;

    fn payload(&self) -> Vec<u8> {
        vec![0x01]
    }
}

/// Disable flash read protection (CMD 0x06, subcommand 0x02).
#[derive(Debug)]
pub struct UnprotectReadFlash;

impl WchLinkCommand for UnprotectReadFlash {
    const COMMAND_ID: CommandId = CommandId::ChipProtect;
    type Response = ();

    fn payload(&self) -> Vec<u8> {
        vec![0x02]
    }
}

/// Query the flash write-protection status (CMD 0x06, subcommand 0x04).
///
/// Answers `0x11` when write protected, `0x00` when unprotected.
#[derive(Debug)]
pub struct CheckWriteProtect;

impl WchLinkCommand for CheckWriteProtect {
    const COMMAND_ID: CommandId = CommandId::ChipProtect;
    type Response = u8;

    fn payload(&self) -> Vec<u8> {
        vec![0x04]
    }
}

/// Disable flash write protection (CMD 0x06, subcommand 0x02).
#[derive(Debug)]
pub struct UnprotectWriteFlash(pub u8);

impl WchLinkCommand for UnprotectWriteFlash {
    const COMMAND_ID: CommandId = CommandId::ChipProtect;
    type Response = ();

    fn payload(&self) -> Vec<u8> {
        vec![0x02, self.0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded<C: WchLinkCommand>(cmd: &C) -> Vec<u8> {
        let mut buf = [0u8; 64];
        let len = cmd.to_bytes(&mut buf).expect("encoding must fit");
        buf[..len].to_vec()
    }

    #[test]
    fn set_rst_pin_bytes() {
        assert_eq!(encoded(&SetRstPin::Low), [0x81, 0x0d, 0x02, 0x0e, 0x13]);
        assert_eq!(
            encoded(&SetRstPin::Floating),
            [0x81, 0x0d, 0x02, 0x0e, 0x15]
        );
    }

    #[test]
    fn response_parsers_reject_malformed_frames() {
        assert!(
            Program::WriteFlash
                .parse_response(&[0x82, 0x03, 1, 2])
                .is_err()
        );
        assert_eq!(
            Program::WriteFlash
                .parse_response(&[0x82, 0x02, 1, 2])
                .unwrap(),
            2
        );
        for len in 0..3 {
            assert!(u8::from_raw(&[0x82, 0x02, 1][..len]).is_err());
        }
    }
}
