use super::util::hex_bytes;
use nom::{
    branch::alt,
    bytes::complete::{tag, take_until, take_while},
    character::complete::char,
    combinator::{opt, peek},
    error::ErrorKind,
    multi::separated_nonempty_list,
    number::complete::hex_u32,
    sequence::preceded,
    IResult,
};

#[derive(Debug, PartialEq, Clone)]
pub enum QueryPacket {
    ThreadId,
    Attached(Option<Pid>),
    Command(Vec<u8>),
    Supported(Vec<String>),
    /// qXfer command
    Transfer {
        object: Vec<u8>,
        operation: TransferOperation,
    },
    HostInfo,
}

#[derive(Debug, PartialEq, Clone)]
pub enum TransferOperation {
    Read {
        annex: Vec<u8>,
        offset: u32,
        length: u32,
    },
    Write {
        annex: Vec<u8>,
        offset: u32,
        data: Vec<u8>,
    },
}

pub type Pid = u32;

/// Parse PID
pub fn pid(input: &[u8]) -> IResult<&[u8], Pid> {
    hex_u32(input)
}

pub fn query_packet(input: &[u8]) -> IResult<&[u8], QueryPacket> {
    let (input, query_packet) = alt((
        query_thread_id,
        query_attached,
        query_command,
        query_supported,
        query_transfer,
        query_hostinfo,
    ))(input)?;

    Ok((input, query_packet))
}

fn query_thread_id(input: &[u8]) -> IResult<&[u8], QueryPacket> {
    let (input, _) = char('C')(input)?;
    Ok((input, QueryPacket::ThreadId))
}

fn query_command(input: &[u8]) -> IResult<&[u8], QueryPacket> {
    let (input, _) = tag("Rcmd,")(input)?;

    let (input, command) = hex_bytes(input)?;
    Ok((input, QueryPacket::Command(command)))
}

fn query_attached(input: &[u8]) -> IResult<&[u8], QueryPacket> {
    let (input, _) = tag("Attached")(input)?;

    let (input, pid) = opt(preceded(char(':'), pid))(input)?;
    Ok((input, QueryPacket::Attached(pid)))
}

fn query_hostinfo(input: &[u8]) -> IResult<&[u8], QueryPacket> {
    let (input, _) = tag("HostInfo")(input)?;

    Ok((input, QueryPacket::HostInfo))
}

fn query_supported(input: &[u8]) -> IResult<&[u8], QueryPacket> {
    let (input, _) = tag("Supported")(input)?;

    let (input, next_char) = peek(opt(char(':')))(input)?;

    // Could also be an empty list
    let (input, features) = if next_char.is_some() {
        let (input, _) = char(':')(input)?;

        separated_nonempty_list(tag(";"), gdb_feature)(input)?
    } else {
        (input, vec![])
    };

    Ok((input, QueryPacket::Supported(features)))
}

fn gdb_feature(input: &[u8]) -> IResult<&[u8], String> {
    let (input, data) = take_while(|c| c != b';')(input)?;

    let feature =
        std::str::from_utf8(data).map_err(|_e| nom::Err::Failure((input, ErrorKind::IsNot)))?;

    // TODO: Actually recognize features with their flags

    Ok((input, feature.to_owned()))
}

fn query_transfer(input: &[u8]) -> IResult<&[u8], QueryPacket> {
    let (input, _) = tag("Xfer")(input)?;

    let (input, _) = char(':')(input)?;

    let (input, object) = take_until(":")(input)?;

    let (input, _) = char(':')(input)?;

    let (input, operation) = alt((transfer_operation_read, transfer_operation_write))(input)?;

    Ok((
        input,
        QueryPacket::Transfer {
            object: object.to_owned(),
            operation,
        },
    ))
}

fn transfer_operation_read(input: &[u8]) -> IResult<&[u8], TransferOperation> {
    let (input, _) = tag("read")(input)?;
    let (input, _) = char(':')(input)?;

    let (input, annex) = take_until(":")(input)?;

    let (input, _) = char(':')(input)?;

    let (input, offset) = hex_u32(input)?;

    let (input, _) = char(',')(input)?;

    let (input, length) = hex_u32(input)?;

    Ok((
        input,
        TransferOperation::Read {
            annex: annex.to_owned(),
            offset,
            length,
        },
    ))
}

fn transfer_operation_write(input: &[u8]) -> IResult<&[u8], TransferOperation> {
    let (input, _) = tag("write")(input)?;
    let (input, annex) = take_until(":")(input)?;

    let (input, _) = char(':')(input)?;

    let (input, offset) = hex_u32(input)?;

    let (input, _) = char(':')(input)?;

    Ok((
        &[],
        TransferOperation::Write {
            annex: annex.to_owned(),
            offset,
            data: input.to_owned(),
        },
    ))
}

#[cfg(test)]
mod test {
    use super::*;

    const EMPTY: &[u8] = &[];

    #[test]
    fn parse_memory_map_read() {
        assert_eq!(
            query_packet(b"Xfer:memory-map:read::1,10").unwrap(),
            (
                EMPTY,
                QueryPacket::Transfer {
                    object: b"memory-map".to_vec(),
                    operation: TransferOperation::Read {
                        annex: vec![],
                        offset: 1,
                        length: 16
                    }
                }
            )
        );
    }

    #[test]
    fn parse_query_supported_example() {
        // Note: Initial q of packet removed
        let packet = b"Supported:multiprocess+;swbreak+;hwbreak+;qRelocInsn+;fork-events+;vfork-events+;exec-events+;vContSupported+;QThreadEvents+;no-resumed+;xmlRegisters=i386";

        assert_eq!(
            query_packet(packet).unwrap(),
            (
                EMPTY,
                QueryPacket::Supported(
                    [
                        "multiprocess+",
                        "swbreak+",
                        "hwbreak+",
                        "qRelocInsn+",
                        "fork-events+",
                        "vfork-events+",
                        "exec-events+",
                        "vContSupported+",
                        "QThreadEvents+",
                        "no-resumed+",
                        "xmlRegisters=i386"
                    ]
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
                )
            )
        );
    }
}
