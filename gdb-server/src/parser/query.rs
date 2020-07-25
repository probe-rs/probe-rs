use nom::{
    branch::alt,
    bytes::complete::{tag, take_until, take_while_m_n},
    character::{complete::char, is_hex_digit},
    combinator::{map_res, opt, peek},
    map,
    multi::{many1, separated_nonempty_list},
    named,
    number::complete::hex_u32,
    recognize,
    sequence::preceded,
    IResult,
};

#[derive(Debug, PartialEq, Clone)]
pub enum QueryPacket {
    ThreadId,
    Attached(Option<Pid>),
    Command(Vec<u8>),
    Supported(Vec<Vec<u8>>),
    /// qXfer command
    Transfer {
        object: Vec<u8>,
        operation: TransferOperation,
    },
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
fn pid(input: &[u8]) -> IResult<&[u8], Pid> {
    hex_u32(input)
}

pub fn query_packet(input: &[u8]) -> IResult<&[u8], QueryPacket> {
    let (input, query_packet) = alt((
        query_thread_id,
        query_attached,
        query_command,
        query_supported,
        query_transfer,
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

fn query_supported(input: &[u8]) -> IResult<&[u8], QueryPacket> {
    let (input, _) = tag("Supported")(input)?;

    let (input, next_char) = peek(char(':'))(input)?;

    // Could also be an empty list
    let (input, features) = if next_char == ':' {
        let (input, _) = char(':')(input)?;

        separated_nonempty_list(tag(";"), gdb_feature)(input)?
    } else {
        (input, vec![])
    };

    Ok((input, QueryPacket::Supported(features)))
}

fn gdb_feature(input: &[u8]) -> IResult<&[u8], Vec<u8>> {
    let (input, data) = take_until(";")(input)?;

    Ok((input, data.to_owned()))
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

    let (input, _) = char(':')(input)?;

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

fn hex_bytes(input: &[u8]) -> IResult<&[u8], Vec<u8>> {
    let (input, bytes) = many1(hex_byte)(input)?;

    Ok((input, bytes))
}

fn hex_byte(input: &[u8]) -> IResult<&[u8], u8> {
    let (input, digits) = take_while_m_n(2, 2, is_hex_digit)(input)?;

    let result = (digits[0] as char).to_digit(16).unwrap_or(0) << 4
        | (digits[1] as char).to_digit(16).unwrap_or(0);

    Ok((input, result as u8))
}

#[cfg(test)]
mod test {
    use super::*;

    const EMPTY: &[u8] = &[];

    #[test]
    fn parse_memory_map_read() {
        assert_eq!(
            query_packet(b"Xfer:memory-map:read::1:10").unwrap(),
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
            (EMPTY, QueryPacket::Supported(vec![]))
        );
    }

    #[test]
    fn parse_hex_bytes() {
        assert_eq!(
            hex_bytes(b"7265736574").unwrap(),
            (EMPTY, vec![0x72, 0x65, 0x73, 0x65, 0x74])
        );
    }

    #[test]
    fn parse_hex_byte() {
        assert_eq!(hex_byte(b"72").unwrap(), (EMPTY, 0x72));

        assert_eq!(hex_byte(b"00").unwrap(), (EMPTY, 0x00));

        assert_eq!(hex_byte(b"FF").unwrap(), (EMPTY, 0xff));

        assert_eq!(hex_byte(b"ab").unwrap(), (EMPTY, 0xab));

        assert_eq!(hex_byte(b"853").unwrap(), ("3".as_bytes(), 0x85));
    }
}
