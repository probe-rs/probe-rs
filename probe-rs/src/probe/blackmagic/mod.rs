//! Black Magic Probe implementation.
use std::{
    char,
    io::{BufReader, BufWriter, Read, Write},
    time::Duration,
};

use crate::{
    architecture::{
        arm::{
            communication_interface::{DapProbe, UninitializedArmProbe},
            ArmCommunicationInterface,
        },
        riscv::{communication_interface::RiscvInterfaceBuilder, dtm::jtag_dtm::JtagDtmBuilder},
        xtensa::communication_interface::{
            XtensaCommunicationInterface, XtensaDebugInterfaceState,
        },
    },
    probe::{DebugProbe, DebugProbeInfo, JTAGAccess, ProbeFactory},
};
use bitvec::{order::Lsb0, vec::BitVec};
use probe_rs_target::ScanChainElement;
use serialport::{available_ports, SerialPortType};

use super::{
    arm_debug_interface::{ProbeStatistics, RawProtocolIo, SwdSettings},
    common::{JtagDriverState, RawJtagIo},
    DebugProbeError, ProbeCreationError, ProbeError, WireProtocol,
};

const BLACK_MAGIC_PROBE_VID: u16 = 0x1d50;
const BLACK_MAGIC_PROBE_PID: u16 = 0x6018;
const BLACK_MAGIC_PROTOCOL_RESPONSE_START: u8 = b'&';
const BLACK_MAGIC_PROTOCOL_RESPONSE_END: u8 = b'#';
pub(crate) const BLACK_MAGIC_REMOTE_SIZE_MAX: usize = 1024;

mod arm;
use arm::UninitializedBlackMagicArmProbe;

/// A factory for creating [`BlackMagicProbe`] instances.
#[derive(Debug)]
pub struct BlackMagicProbeFactory;

#[derive(PartialEq)]
enum ProtocolVersion {
    V0,
    V0P,
    V1,
    V2,
    V3,
    V4,
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum Align {
    U8 = 0,
    U16 = 1,
    U32 = 2,
    U64 = 3,
}

impl core::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::V0 => "V0",
                Self::V0P => "V0P",
                Self::V1 => "V1",
                Self::V2 => "V2",
                Self::V3 => "V3",
                Self::V4 => "V4",
            }
        )
    }
}

#[allow(dead_code)]
#[derive(Debug)]
enum RemoteCommand<'a> {
    Handshake(&'a mut [u8]),
    GetAccelerators,
    HighLevelCheck,
    GetVoltage,
    GetSpeedKhz,
    SetNrst(bool),
    SetPower(bool),
    TargetClockOutput {
        enable: bool,
    },
    SetSpeedKhz(u32),
    SpeedKhz,
    TargetReset(bool),
    RawAccessV0P {
        rnw: u8,
        addr: u8,
        value: u32,
    },
    ReadDpV0P {
        addr: u8,
    },
    ReadApV0P {
        apsel: u8,
        addr: u8,
    },
    WriteApV0P {
        apsel: u8,
        addr: u8,
        value: u32,
    },
    MemReadV0P {
        apsel: u8,
        csw: u32,
        offset: u32,
        data: &'a mut [u8],
    },
    MemWriteV0P {
        apsel: u8,
        csw: u32,
        align: Align,
        offset: u32,
        data: &'a [u8],
    },
    RawAccessV1 {
        index: u8,
        rnw: u8,
        addr: u8,
        value: u32,
    },
    ReadDpV1 {
        index: u8,
        addr: u8,
    },
    ReadApV1 {
        index: u8,
        apsel: u8,
        addr: u8,
    },
    WriteApV1 {
        index: u8,
        apsel: u8,
        addr: u8,
        value: u32,
    },
    MemReadV1 {
        index: u8,
        apsel: u8,
        csw: u32,
        offset: u32,
        data: &'a mut [u8],
    },
    MemWriteV1 {
        index: u8,
        apsel: u8,
        csw: u32,
        align: Align,
        offset: u32,
        data: &'a [u8],
    },
    RawAccessV3 {
        index: u8,
        rnw: u8,
        addr: u8,
        value: u32,
    },
    ReadDpV3 {
        index: u8,
        addr: u8,
    },
    ReadApV3 {
        index: u8,
        apsel: u8,
        addr: u8,
    },
    WriteApV3 {
        index: u8,
        apsel: u8,
        addr: u8,
        value: u32,
    },
    MemReadV3 {
        index: u8,
        apsel: u8,
        csw: u32,
        offset: u32,
        data: &'a mut [u8],
    },
    MemWriteV3 {
        index: u8,
        apsel: u8,
        csw: u32,
        align: Align,
        offset: u32,
        data: &'a [u8],
    },
    MemReadV4 {
        index: u8,
        apsel: u8,
        csw: u32,
        offset: u64,
        data: &'a mut [u8],
    },
    MemWriteV4 {
        index: u8,
        apsel: u8,
        csw: u32,
        align: Align,
        offset: u64,
        data: &'a [u8],
    },
    JtagNext {
        tms: bool,
        tdi: bool,
    },
    JtagTms {
        bits: u32,
        length: usize,
    },
    JtagTdi {
        bits: u32,
        length: usize,
        tms: bool,
    },
    JtagInit,
    JtagReset,
    JtagAddDevice {
        index: u8,
        dr_prescan: u8,
        dr_postscan: u8,
        ir_len: u8,
        ir_prescan: u8,
        ir_postscan: u8,
        current_ir: u32,
    },
    SwdInit,
    SwdIn {
        length: usize,
    },
    SwdInParity {
        length: usize,
    },
    SwdOut {
        value: u32,
        length: usize,
    },
    SwdOutParity {
        value: u32,
        length: usize,
    },
}

impl<'a> RemoteCommand<'a> {
    /// Return the buffer from the payload of the specified value where the
    /// response should be written.
    fn response_buffer(&mut self) -> Option<&mut [u8]> {
        match self {
            RemoteCommand::Handshake(data) => Some(data),
            RemoteCommand::MemReadV0P { data, .. } => Some(data),
            RemoteCommand::MemReadV1 { data, .. } => Some(data),
            RemoteCommand::MemReadV3 { data, .. } => Some(data),
            RemoteCommand::MemReadV4 { data, .. } => Some(data),
            _ => None,
        }
    }

    /// Return `true` if the resulting buffer should have hex decoded.
    fn decode_hex(&self) -> bool {
        matches!(
            self,
            RemoteCommand::MemReadV0P { .. }
                | RemoteCommand::MemReadV1 { .. }
                | RemoteCommand::MemReadV3 { .. }
                | RemoteCommand::MemReadV4 { .. }
        )
    }
}

// Implement `ToString` instead of `Display` as this is for generating
// strings to send over the network, and is not meant for human consumption.
#[allow(clippy::to_string_trait_impl)]
impl<'a> std::string::ToString for RemoteCommand<'a> {
    fn to_string(&self) -> String {
        match self {
            RemoteCommand::Handshake(_) => "+#!GA#".to_string(),
            RemoteCommand::GetVoltage => " !GV#".to_string(),
            RemoteCommand::GetSpeedKhz => "!Gf#".to_string(),
            RemoteCommand::SetSpeedKhz(speed) => {
                format!("!GF{:08x}#", speed)
            }
            RemoteCommand::HighLevelCheck => "!HC#".to_string(),
            RemoteCommand::SetNrst(set) => format!("!GZ{}#", if *set { '1' } else { '0' }),
            RemoteCommand::SetPower(set) => format!("!GP{}#", if *set { '1' } else { '0' }),
            RemoteCommand::TargetClockOutput { enable } => {
                format!("!GE{}#", if *enable { '1' } else { '0' })
            }
            RemoteCommand::SpeedKhz => "!Gf#".to_string(),
            RemoteCommand::RawAccessV0P { rnw, addr, value } => {
                format!("!HL{:02x}{:04x}{:08x}#", rnw, addr, value)
            }
            RemoteCommand::ReadDpV0P { addr } => {
                format!("!Hdff{:04x}#", addr)
            }
            RemoteCommand::ReadApV0P { apsel, addr } => {
                format!("!Ha{:02x}{:04x}#", apsel, 0x100 | *addr as u16)
            }
            RemoteCommand::WriteApV0P { apsel, addr, value } => {
                format!("!HA{:02x}{:04x}{:08x}#", apsel, 0x100 | *addr as u16, value)
            }
            RemoteCommand::MemReadV0P {
                apsel,
                csw,
                offset,
                data,
            } => format!(
                "!HM{:02x}{:08x}{:08x}{:08x}#",
                apsel,
                csw,
                offset,
                data.len()
            ),
            RemoteCommand::MemWriteV0P {
                apsel,
                csw,
                align,
                offset,
                data,
            } => {
                let mut s = format!(
                    "!Hm{:02x}{:08x}{:02x}{:08x}{:08x}",
                    apsel,
                    csw,
                    *align as u8,
                    offset,
                    data.len(),
                );
                for b in data.iter() {
                    s.push_str(&format!("{:02x}", b));
                }
                s.push('#');
                s
            }

            RemoteCommand::RawAccessV1 {
                index,
                rnw,
                addr,
                value,
            } => {
                format!("!HL{:02x}{:02x}{:04x}{:08x}#", index, rnw, addr, value)
            }
            RemoteCommand::ReadDpV1 { index, addr } => {
                format!("!Hd{:02x}ff{:04x}#", index, addr)
            }
            RemoteCommand::ReadApV1 { index, apsel, addr } => {
                format!("!Ha{:02x}{:02x}{:04x}#", index, apsel, 0x100 | *addr as u16)
            }
            RemoteCommand::WriteApV1 {
                index,
                apsel,
                addr,
                value,
            } => format!(
                "!HA{:02x}{:02x}{:04x}{:08x}#",
                index,
                apsel,
                0x100 | *addr as u16,
                value
            ),
            RemoteCommand::MemReadV1 {
                index,
                apsel,
                csw,
                offset,
                data,
            } => format!(
                "!HM{:02x}{:02x}{:08x}{:08x}{:08x}#",
                index,
                apsel,
                csw,
                offset,
                data.len()
            ),
            RemoteCommand::MemWriteV1 {
                index,
                apsel,
                csw,
                align,
                offset,
                data,
            } => {
                let mut s = format!(
                    "!Hm{:02x}{:02x}{:08x}{:02x}{:08x}{:08x}",
                    index,
                    apsel,
                    csw,
                    *align as u8,
                    offset,
                    data.len()
                );
                for b in data.iter() {
                    s.push_str(&format!("{:02x}", b));
                }
                s.push('#');
                s
            }

            RemoteCommand::RawAccessV3 {
                index,
                rnw,
                addr,
                value,
            } => {
                format!("!AR{:02x}{:02x}{:04x}{:08x}#", index, rnw, addr, value)
            }
            RemoteCommand::ReadDpV3 { index, addr } => {
                format!("!Ad{:02x}ff{:04x}#", index, addr)
            }
            RemoteCommand::ReadApV3 { index, apsel, addr } => {
                format!("!Aa{:02x}{:02x}{:04x}#", index, apsel, 0x100 | *addr as u16)
            }
            RemoteCommand::WriteApV3 {
                index,
                apsel,
                addr,
                value,
            } => format!(
                "!AA{:02x}{:02x}{:04x}{:08x}#",
                index,
                apsel,
                0x100 | *addr as u16,
                value.to_be()
            ),
            RemoteCommand::MemReadV3 {
                index,
                apsel,
                csw,
                offset,
                data,
            } => format!(
                "!Am{:02x}{:02x}{:08x}{:08x}{:08x}#",
                index,
                apsel,
                csw,
                offset,
                data.len()
            ),
            RemoteCommand::MemWriteV3 {
                index,
                apsel,
                csw,
                align,
                offset,
                data,
            } => {
                let mut s = format!(
                    "!AM{:02x}{:02x}{:08x}{:02x}{:08x}{:08x}",
                    index,
                    apsel,
                    csw,
                    *align as u8,
                    offset,
                    data.len()
                );
                for b in data.iter() {
                    s.push_str(&format!("{:02x}", b));
                }
                s.push('#');
                s
            }

            RemoteCommand::MemReadV4 {
                index,
                apsel,
                csw,
                offset,
                data,
            } => format!(
                "!Am{:02x}{:02x}{:08x}{:016x}{:08x}#",
                index,
                apsel,
                csw,
                offset,
                data.len()
            ),
            RemoteCommand::MemWriteV4 {
                index,
                apsel,
                csw,
                align,
                offset,
                data,
            } => {
                let mut s = format!(
                    "!AM{:02x}{:02x}{:08x}{:02x}{:016x}{:08x}",
                    index,
                    apsel,
                    csw,
                    *align as u8,
                    offset,
                    data.len()
                );
                for b in data.iter() {
                    s.push_str(&format!("{:02x}", b));
                }
                s.push('#');
                s
            }

            RemoteCommand::JtagNext { tms, tdi } => format!(
                "!JN{}{}#",
                if *tms { '1' } else { '0' },
                if *tdi { '1' } else { '0' }
            ),
            RemoteCommand::JtagInit => "+#!JS#".to_string(),
            RemoteCommand::JtagReset => "+#!JR#".to_string(),
            RemoteCommand::JtagTms { bits, length } => {
                format!("!JT{:02x}{:x}#", *length, *bits)
            }
            RemoteCommand::JtagTdi { bits, length, tms } => format!(
                "!J{}{:02x}{:x}#",
                if *tms { 'D' } else { 'd' },
                *length,
                *bits
            ),
            RemoteCommand::JtagAddDevice {
                index,
                dr_prescan,
                dr_postscan,
                ir_len,
                ir_prescan,
                ir_postscan,
                current_ir,
            } => format!(
                "!HJ{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:08x}#",
                *index, *dr_prescan, *dr_postscan, *ir_len, *ir_prescan, *ir_postscan, *current_ir
            ),
            RemoteCommand::SwdInit => "!SS#".to_string(),
            RemoteCommand::SwdIn { length: bits } => {
                format!("!Si{:02x}#", *bits)
            }
            RemoteCommand::SwdInParity { length } => {
                format!("!SI{:02x}#", *length)
            }
            RemoteCommand::SwdOut { value, length } => {
                format!("!So{:02x}{:x}#", *length, *value)
            }
            RemoteCommand::SwdOutParity { value, length } => {
                format!("!SO{:02x}{:x}#", *length, *value)
            }
            RemoteCommand::TargetReset(reset) => {
                format!("!GZ{}#", if *reset { '1' } else { '0' })
            }
            RemoteCommand::GetAccelerators => "!HA#".to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum RemoteError {
    ParameterError(u64),
    Error(u64),
    Unsupported(u64),
    ProbeError(std::io::Error),
    UnsupportedVersion(u64),
}

impl core::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ParameterError(e) => write!(f, "Remote paramater error with result {:016x}", *e),
            Self::Error(e) => write!(f, "Remote error with result {:016x}", *e),
            Self::Unsupported(e) => write!(f, "Remote command unsupported with result {:016x}", *e),
            Self::ProbeError(e) => write!(f, "Probe error {}", e),
            Self::UnsupportedVersion(e) => write!(f, "Only versions 0-4 are supported, not {}", e),
        }
    }
}

impl ProbeError for RemoteError {}

struct RemoteResponse(u64);

#[derive(PartialEq, Copy, Clone)]
enum SwdDirection {
    Input,
    Output,
}

impl From<bool> for SwdDirection {
    fn from(value: bool) -> Self {
        if value {
            SwdDirection::Output
        } else {
            SwdDirection::Input
        }
    }
}

/// A Black Magic Probe.
pub struct BlackMagicProbe {
    reader: BufReader<Box<dyn Read + Send>>,
    writer: BufWriter<Box<dyn Write + Send>>,
    protocol: Option<WireProtocol>,
    version: String,
    remote_protocol: ProtocolVersion,
    speed_khz: u32,
    jtag_state: JtagDriverState,
    probe_statistics: ProbeStatistics,
    swd_settings: SwdSettings,
    in_bits: BitVec<u8, Lsb0>,
    swd_direction: SwdDirection,
}

impl core::fmt::Debug for BlackMagicProbe {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(
            f,
            "Black Magic Probe {} with remote protocol {}",
            self.version, self.remote_protocol
        )
    }
}

impl core::fmt::Display for BlackMagicProbeFactory {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Black Magic Probe")
    }
}

impl BlackMagicProbe {
    fn new(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
    ) -> Result<Self, DebugProbeError> {
        let mut reader = BufReader::new(reader);
        let mut writer = BufWriter::new(writer);

        let mut handshake_response = [0u8; 1024];
        Self::send(
            &mut writer,
            &RemoteCommand::Handshake(&mut handshake_response),
        )?;
        let response_len = Self::recv(&mut reader, Some(&mut handshake_response), false)
            .map_err(|e| {
                tracing::error!("Unable to receive command: {:?}", e);
                DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::CouldNotOpen)
            })?
            .0;
        let version =
            String::from_utf8_lossy(&handshake_response[0..response_len as usize]).to_string();
        tracing::info!("Probe version {}", version);

        Self::send(&mut writer, &RemoteCommand::HighLevelCheck)?;
        let remote_protocol = if let Ok(response) = Self::recv(&mut reader, None, false) {
            match response.0 {
                0 => ProtocolVersion::V0P,
                1 => ProtocolVersion::V1,
                2 => ProtocolVersion::V2,
                3 => ProtocolVersion::V3,
                4 => ProtocolVersion::V4,
                version => {
                    return Err(DebugProbeError::ProbeCouldNotBeCreated(
                        ProbeCreationError::ProbeSpecific(
                            RemoteError::UnsupportedVersion(version).into(),
                        ),
                    ))
                }
            }
        } else {
            ProtocolVersion::V0
        };

        tracing::info!("Using BMP protocol {}", remote_protocol);

        let mut probe = Self {
            reader,
            writer,
            protocol: None,
            version,
            speed_khz: 0,
            remote_protocol,
            jtag_state: JtagDriverState::default(),
            swd_settings: SwdSettings::default(),
            probe_statistics: ProbeStatistics::default(),
            in_bits: BitVec::new(),
            swd_direction: SwdDirection::Output,
        };

        probe.command(RemoteCommand::SetPower(false)).ok();
        probe.command(RemoteCommand::SetNrst(false)).ok();
        probe.command(RemoteCommand::GetVoltage).ok();
        probe.command(RemoteCommand::SetSpeedKhz(400_0000)).ok();
        probe.command(RemoteCommand::GetSpeedKhz).ok();

        Ok(probe)
    }

    fn command(&mut self, mut command: RemoteCommand) -> Result<RemoteResponse, RemoteError> {
        let result = Self::send(&mut self.writer, &command);
        if let Err(e) = result {
            tracing::error!("Error sending command: {:?}", e);
            return Err(e);
        }
        let should_decode = command.decode_hex();

        Self::recv(&mut self.reader, command.response_buffer(), should_decode)
    }

    fn send(
        writer: &mut BufWriter<Box<dyn Write + Send>>,
        command: &RemoteCommand,
    ) -> Result<(), RemoteError> {
        let s = command.to_string();
        tracing::debug!(" > {}", s);
        write!(writer, "{}", s).map_err(RemoteError::ProbeError)?;
        writer.flush().map_err(RemoteError::ProbeError)
    }

    fn hex_val(c: u8) -> Result<u8, char> {
        match c {
            b'A'..=b'F' => Ok(c - b'A' + 10),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'0'..=b'9' => Ok(c - b'0'),
            _ => Err(c as char),
        }
    }

    fn from_hex_u64(hex: &[u8]) -> Result<u64, ()> {
        // Strip off leading `0x` if present
        let hex = if hex.first() == Some(&b'0')
            && (hex.get(1) == Some(&b'x') || hex.get(1) == Some(&b'X'))
        {
            &hex[2..]
        } else {
            hex
        };

        let mut val = 0u64;

        for (index, c) in hex.iter().rev().enumerate() {
            let decoded_val: u64 = Self::hex_val(*c).or(Err(()))?.into();
            val += decoded_val << (index * 4);
        }
        Ok(val)
    }

    fn recv_u64(reader: &mut BufReader<Box<dyn Read + Send>>) -> Result<u64, RemoteError> {
        let mut response_buffer = [0u8; 16];
        let mut response_len = 0;
        for dest in response_buffer.iter_mut() {
            let mut byte = [0u8; 1];
            reader
                .read_exact(&mut byte)
                .map_err(RemoteError::ProbeError)?;
            if byte[0] == BLACK_MAGIC_PROTOCOL_RESPONSE_END {
                break;
            }
            *dest = byte[0];
            response_len += 1;
        }
        // Convert the hex in the buffer to a u64.
        Self::from_hex_u64(&response_buffer[0..response_len])
            .or(Err(RemoteError::ParameterError(0)))
    }

    fn recv(
        reader: &mut BufReader<Box<dyn Read + Send>>,
        buffer: Option<&mut [u8]>,
        decode_hex: bool,
    ) -> Result<RemoteResponse, RemoteError> {
        // Responses begin with `&`
        loop {
            let mut byte = [0u8; 1];
            reader
                .read_exact(&mut byte)
                .map_err(RemoteError::ProbeError)?;
            if byte[0] == BLACK_MAGIC_PROTOCOL_RESPONSE_START {
                break;
            }
        }
        let mut response_code = [0u8; 1];
        reader
            .read_exact(&mut response_code)
            .map_err(RemoteError::ProbeError)?;
        let response_code = response_code[0];

        // If there was no incoming buffer, then we will read up to 64 bits of data and
        // return a response based on that.

        if response_code == b'K' {
            let Some(buffer) = buffer else {
                let response = Self::recv_u64(reader)?;
                tracing::trace!(" < K{:x}", response);
                return Ok(RemoteResponse(response));
            };
            let mut output_count = 0;
            for dest in buffer.iter_mut() {
                let mut byte = [0u8; 1];

                // Read the first nibble
                reader
                    .read_exact(&mut byte)
                    .map_err(RemoteError::ProbeError)?;
                if byte[0] == BLACK_MAGIC_PROTOCOL_RESPONSE_END {
                    break;
                }

                // Add one byte to the resulting output value. This is the case whether we
                // get one or two nibbles.
                output_count += 1;

                if decode_hex {
                    *dest = Self::hex_val(byte[0])
                        .or(Err(RemoteError::ParameterError(byte[0] as _)))?;

                    // Read the second nibble, if present.
                    reader
                        .read_exact(&mut byte)
                        .map_err(RemoteError::ProbeError)?;
                    if byte[0] == BLACK_MAGIC_PROTOCOL_RESPONSE_END {
                        break;
                    }

                    *dest = *dest << 4
                        | Self::hex_val(byte[0])
                            .or(Err(RemoteError::ParameterError(byte[0] as _)))?;
                } else {
                    *dest = byte[0];
                }
            }
            tracing::trace!(" < K{:x?}", &buffer[0..output_count as usize]);
            Ok(RemoteResponse(output_count))
        } else {
            let response = Self::recv_u64(reader)?;
            tracing::trace!(" < {}{:x}", char::from(response_code), response);
            if response_code == b'E' {
                Err(RemoteError::Error(response))
            } else if response_code == b'P' {
                Err(RemoteError::ParameterError(response))
            } else {
                Err(RemoteError::Unsupported(response))
            }
        }
    }

    fn get_speed(&mut self) -> Result<u32, DebugProbeError> {
        let speed = self.command(RemoteCommand::SpeedKhz)?.0.try_into().unwrap();
        Ok(speed)
    }

    fn drain_swd_accumulator(
        &mut self,
        output: &mut Vec<bool>,
        accumulator: u32,
        accumulator_length: usize,
    ) -> Result<(), DebugProbeError> {
        if self.swd_direction == SwdDirection::Output {
            match self.command(RemoteCommand::SwdOut {
                value: accumulator,
                length: accumulator_length,
            }) {
                Ok(response) => tracing::debug!(
                    "Doing SWD out of {} bits: {:x} -- {}",
                    accumulator_length,
                    accumulator,
                    response.0
                ),
                Err(e) => tracing::error!(
                    "Error doing SWD OUT of {} bits ({:x}) -- {}",
                    accumulator_length,
                    accumulator,
                    e
                ),
            }
            for bit in 0..accumulator_length {
                output.push(accumulator & (1 << bit) != 0);
            }
        } else {
            let result = self.command(RemoteCommand::SwdIn {
                length: accumulator_length,
            });
            match &result {
                Ok(response) => {
                    let response = response.0;
                    tracing::debug!(
                        "Doing SWD in of {} bits: {:x}",
                        accumulator_length,
                        response
                    );
                    for bit in 0..accumulator_length {
                        output.push(response & (1 << bit) != 0);
                    }
                }
                Err(e) => tracing::error!(
                    "Error doing SWD IN operation of {} bits: {}",
                    accumulator_length,
                    e
                ),
            }
        }
        Ok(())
    }

    /// Perform a single SWDIO command
    ///
    /// The caller needs to ensure that the given iterators are not longer than the maximum transfer size
    /// allowed. It seems that the maximum transfer size is determined by [`self.max_mem_block_size`].
    fn perform_swdio_transfer<D, S>(
        &mut self,
        dir: D,
        swdio: S,
    ) -> Result<Vec<bool>, DebugProbeError>
    where
        D: IntoIterator<Item = bool>,
        S: IntoIterator<Item = bool>,
    {
        let dir = dir.into_iter();
        let swdio = swdio.into_iter();
        let mut output = vec![];

        let mut accumulator = 0u32;
        let mut accumulator_length = 0;

        for (dir, swdio) in dir.zip(swdio) {
            let dir = SwdDirection::from(dir);
            if dir != self.swd_direction
                || accumulator_length >= core::mem::size_of_val(&accumulator) * 8
            {
                // Inputs are off-by-one due to how J-Link is built. Remove one bit
                // from the accumulator and store the turnaround bit at the end of
                // the transaction.
                if self.swd_direction == SwdDirection::Input && dir == SwdDirection::Output {
                    accumulator_length -= 2;
                }

                // Drain the accumulator to the BMP, either writing bits to the device
                // or reading bits from the device.
                self.drain_swd_accumulator(&mut output, accumulator, accumulator_length)?;

                // Input -> Output transition
                if self.swd_direction == SwdDirection::Input && dir == SwdDirection::Output {
                    output.push(false);
                    output.push(false);
                }

                accumulator = 0;
                accumulator_length = 0;
            }
            self.swd_direction = dir;
            accumulator |= if swdio { 1 << accumulator_length } else { 0 };
            accumulator_length += 1;
        }

        if accumulator_length > 0 {
            self.drain_swd_accumulator(&mut output, accumulator, accumulator_length)?;
        }

        Ok(output)
    }

    fn drain_jtag_accumulator(
        &mut self,
        accumulator: u32,
        mut accumulator_length: usize,
        final_tms: bool,
        capture: bool,
        final_transaction: bool,
    ) -> Result<(), DebugProbeError> {
        let response = self.command(RemoteCommand::JtagTdi {
            bits: accumulator,
            length: accumulator_length,
            tms: final_tms && final_transaction,
        })?;

        if capture {
            // If this is the last bit, then `cap` may be false.
            if capture && final_transaction {
                accumulator_length -= 1;
            }
            let value = response.0;
            for bit in 0..accumulator_length {
                self.in_bits.push(value & (1 << bit) != 0);
            }
        }
        Ok(())
    }

    fn perform_jtag_transfer(
        &mut self,
        transaction: Vec<(bool, bool, bool)>,
        final_tms: bool,
        capture: bool,
    ) -> Result<(), DebugProbeError> {
        let mut accumulator = 0;
        let mut accumulator_length = 0;
        let bit_count = transaction.len();

        for (index, (_, tdi, _)) in transaction.into_iter().enumerate() {
            accumulator |= if tdi { 1 << accumulator_length } else { 0 };
            accumulator_length += 1;
            if accumulator_length >= core::mem::size_of_val(&accumulator) * 8 {
                let is_final = index + 1 >= bit_count;
                self.drain_jtag_accumulator(
                    accumulator,
                    accumulator_length,
                    final_tms,
                    capture,
                    is_final,
                )?;
                accumulator = 0;
                accumulator_length = 0;
            }
        }

        if accumulator_length > 0 {
            self.drain_jtag_accumulator(accumulator, accumulator_length, final_tms, capture, true)?;
        }
        Ok(())
    }
}

impl DebugProbe for BlackMagicProbe {
    fn get_name(&self) -> &str {
        "Black Magic probe"
    }

    fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        Self::send(&mut self.writer, &RemoteCommand::SetSpeedKhz(speed_khz))?;
        self.speed_khz = self.get_speed()?;
        Ok(self.speed_khz)
    }

    fn set_scan_chain(&mut self, scan_chain: Vec<ScanChainElement>) -> Result<(), DebugProbeError> {
        tracing::info!("Setting scan chain to {:?}", scan_chain);
        self.jtag_state.expected_scan_chain = Some(scan_chain);
        Ok(())
    }

    fn scan_chain(&self) -> Result<&[ScanChainElement], DebugProbeError> {
        if let Some(scan_chain) = &self.jtag_state.expected_scan_chain {
            Ok(scan_chain)
        } else {
            Ok(&[])
        }
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Attaching with protocol '{:?}'", self.protocol);

        // Enable output on the clock pin (if supported)
        if let ProtocolVersion::V2 | ProtocolVersion::V3 | ProtocolVersion::V4 =
            self.remote_protocol
        {
            self.command(RemoteCommand::TargetClockOutput { enable: true })
                .ok();
        }

        match self.protocol {
            Some(WireProtocol::Jtag) => {
                self.scan_chain()?;
                self.select_target(0)?;

                if let ProtocolVersion::V1
                | ProtocolVersion::V2
                | ProtocolVersion::V3
                | ProtocolVersion::V4 = self.remote_protocol
                {
                    let sc = &self.jtag_state.chain_params;
                    self.command(RemoteCommand::JtagAddDevice {
                        index: 0,
                        dr_prescan: sc.drpre.try_into().unwrap(),
                        dr_postscan: sc.drpost.try_into().unwrap(),
                        ir_len: sc.irlen.try_into().unwrap(),
                        ir_prescan: sc.irpre.try_into().unwrap(),
                        ir_postscan: sc.irpost.try_into().unwrap(),
                        current_ir: u32::MAX,
                    })?;
                }
                Ok(())
            }
            Some(WireProtocol::Swd) => Ok(()),
            _ => Err(DebugProbeError::InterfaceNotAvailable {
                interface_name: "no protocol specified",
            }),
        }
    }

    fn select_jtag_tap(&mut self, index: usize) -> Result<(), DebugProbeError> {
        self.select_target(index)
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        // TODO we could add this by using a GPIO. However, different probes may connect
        // different pins (if any) to the reset line, so we would need to make this configurable.
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset",
        })
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.command(RemoteCommand::TargetReset(true))?;
        Ok(())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.command(RemoteCommand::TargetReset(false))?;
        Ok(())
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        self.protocol = Some(protocol);

        tracing::debug!("Switching to protocol {}", protocol);
        match protocol {
            WireProtocol::Jtag => {
                self.command(RemoteCommand::JtagInit)?;
                self.command(RemoteCommand::JtagReset)?;
            }
            WireProtocol::Swd => {
                self.command(RemoteCommand::SwdInit)?;
            }
        }
        Ok(())
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        self.protocol
    }

    fn try_get_riscv_interface_builder<'probe>(
        &'probe mut self,
    ) -> Result<Box<dyn RiscvInterfaceBuilder<'probe> + 'probe>, DebugProbeError> {
        Ok(Box::new(JtagDtmBuilder::new(self)))
    }

    fn has_riscv_interface(&self) -> bool {
        true
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    /// Turn this probe into an ARM probe
    fn try_get_arm_interface<'probe>(
        mut self: Box<Self>,
    ) -> Result<Box<dyn UninitializedArmProbe + 'probe>, (Box<dyn DebugProbe>, DebugProbeError)>
    {
        let has_adiv5 = match self.remote_protocol {
            ProtocolVersion::V0 => false,
            ProtocolVersion::V0P
            | ProtocolVersion::V1
            | ProtocolVersion::V2
            | ProtocolVersion::V3 => true,
            ProtocolVersion::V4 => {
                if let Ok(accelerators) = self.command(RemoteCommand::GetAccelerators) {
                    accelerators.0 & 1 != 0
                } else {
                    false
                }
            }
        };

        if has_adiv5 {
            Ok(Box::new(UninitializedBlackMagicArmProbe::new(self)))
        } else {
            Ok(Box::new(ArmCommunicationInterface::new(self, true)))
        }
    }

    fn has_arm_interface(&self) -> bool {
        true
    }

    fn try_get_xtensa_interface<'probe>(
        &'probe mut self,
        state: &'probe mut XtensaDebugInterfaceState,
    ) -> Result<XtensaCommunicationInterface<'probe>, DebugProbeError> {
        Ok(XtensaCommunicationInterface::new(self, state))
    }

    fn has_xtensa_interface(&self) -> bool {
        true
    }
}

impl DapProbe for BlackMagicProbe {}

impl RawProtocolIo for BlackMagicProbe {
    fn jtag_shift_tms<M>(&mut self, tms: M, _tdi: bool) -> Result<(), DebugProbeError>
    where
        M: IntoIterator<Item = bool>,
    {
        self.probe_statistics.report_io();

        let tms = tms.into_iter().collect::<Vec<bool>>();
        let mut accumulator = 0;
        let mut accumulator_length = 0;
        for tms in tms.into_iter() {
            accumulator |= if tms { 1 << accumulator_length } else { 0 };
            accumulator_length += 1;

            if accumulator_length >= core::mem::size_of_val(&accumulator) * 8 {
                self.command(RemoteCommand::JtagTms {
                    bits: accumulator,
                    length: accumulator_length,
                })?;
                accumulator_length = 0;
                accumulator = 0;
            }
        }

        if accumulator_length > 0 {
            self.command(RemoteCommand::JtagTms {
                bits: accumulator,
                length: accumulator_length,
            })?;
        }

        Ok(())
    }

    fn jtag_shift_tdi<I>(&mut self, _tms: bool, tdi: I) -> Result<(), DebugProbeError>
    where
        I: IntoIterator<Item = bool>,
    {
        self.probe_statistics.report_io();

        let tdi = tdi.into_iter().collect::<Vec<bool>>();
        let mut accumulator = 0;
        let mut accumulator_length = 0;
        for tms in tdi.into_iter() {
            accumulator |= if tms { 1 << accumulator_length } else { 0 };
            accumulator_length += 1;

            if accumulator_length >= core::mem::size_of_val(&accumulator) * 8 {
                self.command(RemoteCommand::JtagTdi {
                    bits: accumulator,
                    length: accumulator_length,
                    tms: false,
                })?;
                accumulator_length = 0;
                accumulator = 0;
            }
        }

        if accumulator_length > 0 {
            self.command(RemoteCommand::JtagTdi {
                bits: accumulator,
                length: accumulator_length,
                tms: false,
            })?;
        }

        Ok(())
    }

    fn swd_io<D, S>(&mut self, dir: D, swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        D: IntoIterator<Item = bool>,
        S: IntoIterator<Item = bool>,
    {
        self.probe_statistics.report_io();
        self.perform_swdio_transfer(dir, swdio)
    }

    fn swj_pins(
        &mut self,
        _pin_out: u32,
        _pin_select: u32,
        _pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        Err(DebugProbeError::CommandNotSupportedByProbe {
            command_name: "swj_pins",
        })
    }

    fn swd_settings(&self) -> &SwdSettings {
        &self.swd_settings
    }

    fn probe_statistics(&mut self) -> &mut ProbeStatistics {
        &mut self.probe_statistics
    }
}

impl RawJtagIo for BlackMagicProbe {
    fn shift_bit(
        &mut self,
        tms: bool,
        tdi: bool,
        capture_tdo: bool,
    ) -> Result<(), DebugProbeError> {
        self.jtag_state.state.update(tms);
        let response = self.command(RemoteCommand::JtagNext { tms, tdi }).unwrap();
        if capture_tdo {
            self.in_bits.push(response.0 != 0);
        }
        Ok(())
    }

    fn shift_bits(
        &mut self,
        tms: impl IntoIterator<Item = bool>,
        tdi: impl IntoIterator<Item = bool>,
        cap: impl IntoIterator<Item = bool>,
    ) -> Result<(), DebugProbeError> {
        let mut transaction = vec![];
        let mut last_tms = false;
        let mut last_cap = false;

        let mut special_transaction = false;
        let mut tms_true_count = 0;
        let mut cap_count = 0;
        for (tms, (tdi, cap)) in tms.into_iter().zip(tdi.into_iter().zip(cap)) {
            if tms {
                tms_true_count += 1;
            }
            if cap {
                cap_count += 1;
            }
            last_tms = tms;
            last_cap = cap;
            transaction.push((tms, tdi, cap));
        }

        // A strange number of bits are captured, such as including the last bit or
        // including a smattering of bits in the middle of the transaction.
        if (cap_count != 0 && (cap_count + 1 != transaction.len())) || last_cap {
            special_transaction = true;
        }

        // The TMS value is `true` for a field other than the last bit
        if tms_true_count > 1 || (tms_true_count == 1 && !last_tms) {
            special_transaction = true;
        }

        if special_transaction {
            for (tms, tdi, cap) in transaction {
                self.shift_bit(tms, tdi, cap)?;
            }
        } else {
            self.jtag_state.state.update(tms_true_count > 0);
            self.perform_jtag_transfer(transaction, tms_true_count > 0, cap_count > 0)?;
        }

        Ok(())
    }

    fn read_captured_bits(&mut self) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        tracing::trace!("reading captured bits");
        Ok(std::mem::take(&mut self.in_bits))
    }

    fn state_mut(&mut self) -> &mut JtagDriverState {
        &mut self.jtag_state
    }

    fn state(&self) -> &JtagDriverState {
        &self.jtag_state
    }
}

/// Determine if a given serial port is a Black Magic Probe GDB interface.
/// The BMP has at least two serial ports, and we want to make sure we get
/// the correct one.
fn black_magic_debug_port_info(
    port_type: SerialPortType,
    port_name: &str,
) -> Option<DebugProbeInfo> {
    // Only accept /dev/cu.* values on macos, to avoid having two
    // copies of the port (both /dev/tty.* and /dev/cu.*)
    if cfg!(target_os = "macos") && !port_name.contains("/cu.") {
        return None;
    }

    let (vendor_id, product_id, serial_number, hid_interface, identifier) = match port_type {
        SerialPortType::UsbPort(info) => (
            info.vid,
            info.pid,
            info.serial_number.map(|s| s.to_string()),
            info.interface,
            info.product
                .unwrap_or_else(|| "Black Magic Probe".to_string()),
        ),
        _ => return None,
    };

    if vendor_id != BLACK_MAGIC_PROBE_VID {
        return None;
    }
    if product_id != BLACK_MAGIC_PROBE_PID {
        return None;
    }

    // Mac specifies the interface as the CDC Data interface, whereas Linux and
    // Windows use the CDC Communications interface. Accept either one here.
    if hid_interface != Some(0) && hid_interface != Some(1) {
        return None;
    }

    Some(DebugProbeInfo {
        identifier,
        vendor_id,
        product_id,
        serial_number,
        probe_factory: &BlackMagicProbeFactory,
        hid_interface,
    })
}

impl ProbeFactory for BlackMagicProbeFactory {
    fn open(
        &self,
        selector: &super::DebugProbeSelector,
    ) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        // Ensure the VID and PID match Black Magic Probes
        if selector.vendor_id != BLACK_MAGIC_PROBE_VID
            || selector.product_id != BLACK_MAGIC_PROBE_PID
        {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::NotFound,
            ));
        }

        // If the serial number is a valid "address:port" string, attempt to
        // connect to it via TCP.
        if let Some(serial_number) = &selector.serial_number {
            if let Ok(connection) = std::net::TcpStream::connect(serial_number) {
                let reader = connection;
                let writer = reader.try_clone().map_err(|e| {
                    DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::Usb(e))
                })?;
                return BlackMagicProbe::new(Box::new(reader), Box::new(writer))
                    .map(|p| Box::new(p) as Box<dyn DebugProbe>);
            }
        }

        // Otherwise, treat it as a serial port and iterate through all ports.
        let Ok(ports) = available_ports() else {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::CouldNotOpen,
            ));
        };

        for port_description in ports {
            let Some(info) = black_magic_debug_port_info(
                port_description.port_type,
                &port_description.port_name,
            ) else {
                continue;
            };

            if selector.serial_number != info.serial_number {
                continue;
            }

            // Open with the baud rate 115200. This baud rate is arbitrary, since it's
            // a soft USB device and will run at the same speed regardless of the baud rate.
            let mut port = serialport::new(port_description.port_name, 115200)
                .timeout(std::time::Duration::from_secs(1))
                .open()
                .map_err(|_| {
                    DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::CouldNotOpen)
                })?;

            // Set DTR, indicating we're ready to communicate.
            port.write_data_terminal_ready(true).map_err(|_| {
                DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::CouldNotOpen)
            })?;
            // A delay appears necessary to allow the BMP to recognize the DTR signal.
            std::thread::sleep(Duration::from_millis(250));
            let reader = port;
            let writer = reader.try_clone().map_err(|_| {
                DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::CouldNotOpen)
            })?;
            return BlackMagicProbe::new(Box::new(reader), Box::new(writer))
                .map(|p| Box::new(p) as Box<dyn DebugProbe>);
        }

        Err(DebugProbeError::ProbeCouldNotBeCreated(
            ProbeCreationError::NotFound,
        ))
    }

    fn list_probes(&self) -> Vec<super::DebugProbeInfo> {
        let mut probes = vec![];
        let Ok(ports) = available_ports() else {
            return probes;
        };
        for port in ports {
            let Some(info) = black_magic_debug_port_info(port.port_type, &port.port_name) else {
                continue;
            };
            probes.push(info);
        }
        probes
    }
}
