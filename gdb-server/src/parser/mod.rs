//! Parser for GDB packets
//!
//! GDB packets have the format `$packet-data#checksum`. This parser is
//! focused on the actual packet-data.
pub(crate) mod query;
mod util;
pub(crate) mod v_packet;

use nom::{
    branch::alt,
    bytes::complete::{tag, take},
    character::complete::char,
    combinator::value,
    map,
    multi::many0,
    named,
    number::complete::hex_u32,
    IResult,
};

use anyhow::{anyhow, Result};
use query::query_packet;
use v_packet::v_packet;

pub use query::{Pid, QueryPacket};
use util::{hex_u32_le, hex_u64};
pub use v_packet::VPacket;

#[allow(dead_code)]
#[derive(Debug, PartialEq, Clone)]
pub enum Packet {
    /// Packet `!`
    EnableExtendedMode,
    /// Packet `?`
    HaltReason,
    /// Packet  `A`
    Arguments,
    /// Packet `b`.
    ///
    /// Not recommended
    Baud,
    /// Packet `B`
    ///
    /// Not recommended. Use `Z` and `z` instead.
    Breakpoint,
    /// Packet `bc`
    BackwardContinue,
    /// Packet `bs`
    BackwardSingleStep,
    /// Packet `c`
    Continue,
    /// Packet `C`
    ContinueSignal,
    /// Packet `d`
    Debug,
    /// Packet `D`
    Detach,
    /// Packet `F`
    FileIO,
    /// Packet `g`
    ReadGeneralRegister,
    /// Packet `G`
    WriteGeneralRegister {
        reg_values: Vec<u32>,
    },
    /// Packet `H`
    SelectThread,
    /// Packet `i`
    StepClockCycle,
    /// Packet `I`
    StepClockCycleSignal,
    /// Packet `k`
    KillRequest,
    /// Packet 'm'
    ReadMemory {
        address: u64,
        length: u32,
    },
    /// Packet 'M'
    WriteMemory,
    /// Packet 'p'
    ReadRegisterHex(u32),
    /// Packet 'P'
    WriteRegisterHex {
        address: u32,
        value: u32,
    },
    // Packet 'q'
    Query(QueryPacket),
    // Packet 'Q'
    QuerySet,
    // Packet 'r'
    Reset,
    // Packet 'R'
    Restart,
    // Packet 's'
    SingleStep,
    // Packet 's'
    SingleStepSignal,
    // Packet 't'
    SearchBackwards,
    // Packet 'T'
    ThreadInfo,
    // Packet 'v'
    V(VPacket),
    // Packet 'X'
    WriteMemoryBinary {
        address: u32,
        data: Vec<u8>,
    },
    // Packet 'z'
    RemoveBreakpoint {
        breakpoint_type: BreakpointType,
        address: u32,
        kind: u32,
    },
    // Packet 'Z'
    InsertBreakpoint {
        breakpoint_type: BreakpointType,
        address: u32,
        kind: u32,
    },
    // Byte 0x03
    Interrupt,
}

#[derive(Debug, PartialEq, Clone)]
pub enum BreakpointType {
    Software,
    Hardware,
    WriteWatchpoint,
    ReadWatchpoint,
    AccessWatchpoint,
}

pub fn parse_packet(input: &[u8]) -> Result<Packet> {
    let parse_result = alt((
        extended_mode,
        detach,
        halt_reason,
        read_register,
        read_register_hex,
        read_memory,
        query,
        v,
        insert_breakpoint,
        remove_breakpoint,
        write_memory_binary,
        ctrl_c_interrupt,
        continue_packet,
        write_register,
        write_register_hex,
    ))(input);

    match parse_result {
        Ok((_remaining, packet)) => Ok(packet),
        Err(e) => Err(anyhow!("{}", e)),
    }
}

named!(extended_mode<&[u8], Packet>, map!(char('!'), |_| Packet::EnableExtendedMode));

named!(halt_reason<&[u8], Packet>, map!(char('?'), |_| Packet::HaltReason));

fn continue_packet(input: &[u8]) -> IResult<&[u8], Packet> {
    let (input, _) = char('c')(input)?;

    Ok((input, Packet::Continue))
}

fn detach(input: &[u8]) -> IResult<&[u8], Packet> {
    value(Packet::Detach, char('D'))(input)
}

fn read_register(input: &[u8]) -> IResult<&[u8], Packet> {
    let (input, _) = char('g')(input)?;

    Ok((input, Packet::ReadGeneralRegister))
}

fn read_register_hex(input: &[u8]) -> IResult<&[u8], Packet> {
    let (input, _) = char('p')(input)?;

    let (input, value) = hex_u32(input)?;

    Ok((input, Packet::ReadRegisterHex(value)))
}

fn write_register(input: &[u8]) -> IResult<&[u8], Packet> {
    let (input, _) = char('G')(input)?;

    // TODO: Handle target byteorder correctly
    let (input, v) = many0(hex_u32_le)(input)?;

    Ok((input, Packet::WriteGeneralRegister { reg_values: v }))
}

fn write_register_hex(input: &[u8]) -> IResult<&[u8], Packet> {
    let (input, _) = char('P')(input)?;

    let (input, address) = hex_u32(input)?;

    let (input, _) = char('=')(input)?;

    // TODO: Handle target byteorder correctly
    let (input, value) = hex_u32_le(input)?;

    Ok((input, Packet::WriteRegisterHex { address, value }))
}

fn query(input: &[u8]) -> IResult<&[u8], Packet> {
    let (input, _) = char('q')(input)?;
    let (input, packet) = query_packet(input)?;

    Ok((input, Packet::Query(packet)))
}

fn v(input: &[u8]) -> IResult<&[u8], Packet> {
    let (input, _) = char('v')(input)?;

    let (input, packet) = v_packet(input)?;

    Ok((input, Packet::V(packet)))
}

fn read_memory(input: &[u8]) -> IResult<&[u8], Packet> {
    let (input, _) = char('m')(input)?;

    let (input, address) = hex_u64(input)?;
    let (input, _) = char(',')(input)?;
    let (input, length) = hex_u32(input)?;

    Ok((input, Packet::ReadMemory { address, length }))
}

fn breakpoint_type(input: &[u8]) -> IResult<&[u8], BreakpointType> {
    alt((
        value(BreakpointType::Software, char('0')),
        value(BreakpointType::Hardware, char('1')),
        value(BreakpointType::WriteWatchpoint, char('2')),
        value(BreakpointType::ReadWatchpoint, char('3')),
        value(BreakpointType::AccessWatchpoint, char('4')),
    ))(input)
}

fn insert_breakpoint(input: &[u8]) -> IResult<&[u8], Packet> {
    let (input, _) = char('Z')(input)?;

    let (input, breakpoint_type) = breakpoint_type(input)?;

    let (input, _) = char(',')(input)?;

    let (input, address) = hex_u32(input)?;

    let (input, _) = char(',')(input)?;

    let (input, kind) = hex_u32(input)?;

    Ok((
        input,
        Packet::InsertBreakpoint {
            breakpoint_type,
            address,
            kind,
        },
    ))
}

fn remove_breakpoint(input: &[u8]) -> IResult<&[u8], Packet> {
    let (input, _) = char('z')(input)?;
    let (input, breakpoint_type) = breakpoint_type(input)?;

    let (input, _) = char(',')(input)?;

    let (input, address) = hex_u32(input)?;
    let (input, _) = char(',')(input)?;

    let (input, kind) = hex_u32(input)?;

    Ok((
        input,
        Packet::RemoveBreakpoint {
            breakpoint_type,
            address,
            kind,
        },
    ))
}

fn write_memory_binary(input: &[u8]) -> IResult<&[u8], Packet> {
    let (input, _) = char('X')(input)?;

    let (input, address) = hex_u32(input)?;
    let (input, _) = char(',')(input)?;
    let (input, length) = hex_u32(input)?;
    let (input, _) = char(':')(input)?;

    let (input, data) = take(length)(input)?;

    Ok((
        input,
        Packet::WriteMemoryBinary {
            address,
            data: data.to_owned(),
        },
    ))
}

fn ctrl_c_interrupt(input: &[u8]) -> IResult<&[u8], Packet> {
    let (input, _) = tag([0x03])(input)?;

    Ok((input, Packet::Interrupt))
}

#[cfg(test)]
mod test {
    use super::*;
    use query::TransferOperation;

    const EMPTY: &[u8] = &[];

    #[test]
    fn parse_simple_packets() {
        let test_data = [
            ("!", Packet::EnableExtendedMode),
            ("?", Packet::HaltReason),
            ("c", Packet::Continue),
            ("g", Packet::ReadGeneralRegister),
            ("D", Packet::Detach),
            ("qSupported", Packet::Query(QueryPacket::Supported(vec![]))),
            ("qHostInfo", Packet::Query(QueryPacket::HostInfo)),
            ("vCont?", Packet::V(VPacket::QueryContSupport)),
            (
                "vMustReplyEmpty",
                Packet::V(VPacket::Unknown("MustReplyEmpty".into())),
            ),
        ];

        for (input, expected) in test_data.iter() {
            let parsed = parse_packet(input.as_bytes());

            assert!(parsed.is_ok(), "Failed to parse '{}'", input);

            assert_eq!(parsed.unwrap(), *expected);
        }
    }

    #[test]
    fn parse_packet_read_register_hex() {
        assert_eq!(parse_packet(b"p03").unwrap(), Packet::ReadRegisterHex(3));
    }

    #[test]
    fn parse_query_attached() {
        assert_eq!(
            query(b"qAttached").unwrap(),
            (EMPTY, Packet::Query(QueryPacket::Attached(None)))
        );
    }

    #[test]
    fn parse_query_attached_with_pid() {
        assert_eq!(
            query(b"qAttached:02").unwrap(),
            (EMPTY, Packet::Query(QueryPacket::Attached(Some(2))))
        );
    }

    #[test]
    fn parse_query_command() {
        assert_eq!(
            query(b"qRcmd,7265736574").unwrap(),
            (
                EMPTY,
                Packet::Query(QueryPacket::Command(vec![0x72, 0x65, 0x73, 0x65, 0x74]))
            )
        );
    }

    #[test]
    fn parse_read_register_hex() {
        assert_eq!(
            read_register_hex(b"p00").unwrap(),
            (EMPTY, Packet::ReadRegisterHex(0))
        );
    }

    #[test]
    fn parse_read_memory() {
        assert_eq!(
            parse_packet(b"m004512,07").unwrap(),
            Packet::ReadMemory {
                address: 0x4512,
                length: 0x07,
            }
        );
    }

    #[test]
    fn parse_read_memory_long_address() {
        assert_eq!(
            parse_packet(b"mffffff8000002010,8").unwrap(),
            Packet::ReadMemory {
                address: 0xffffff8000002010,
                length: 0x8,
            }
        );
    }

    #[test]
    fn parse_insert_breakpoint() {
        assert_eq!(
            parse_packet(b"Z0,3456,2").unwrap(),
            Packet::InsertBreakpoint {
                breakpoint_type: BreakpointType::Software,
                address: 0x3456,
                kind: 0x2,
            }
        );
    }

    #[test]
    fn parse_remove_breakpoint() {
        assert_eq!(
            parse_packet(b"z1,274,0").unwrap(),
            Packet::RemoveBreakpoint {
                breakpoint_type: BreakpointType::Hardware,
                address: 0x274,
                kind: 0,
            }
        );
    }

    #[test]
    fn parse_write_memory_binary() {
        assert_eq!(
            parse_packet(b"X270,7:.sd223!").unwrap(),
            Packet::WriteMemoryBinary {
                address: 0x270,
                data: b".sd223!".to_vec()
            }
        );
    }

    #[test]
    fn parse_interrupt() {
        assert_eq!(parse_packet(&[0x03]).unwrap(), Packet::Interrupt);
    }

    #[test]
    fn parse_memory_map_read() {
        assert_eq!(
            parse_packet(b"qXfer:memory-map:read::0,2047").unwrap(),
            Packet::Query(QueryPacket::Transfer {
                object: b"memory-map".to_vec(),
                operation: TransferOperation::Read {
                    annex: vec![],
                    offset: 0,
                    length: 0x2047,
                }
            })
        );
    }

    #[test]
    fn parse_query_crc_packet() {
        assert_eq!(
            parse_packet(b"qCRC:8000000,13c").unwrap(),
            Packet::Query(QueryPacket::Crc {
                address: 0x8000000,
                length: 0x13c,
            })
        );
    }
}
