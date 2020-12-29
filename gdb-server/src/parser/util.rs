use nom::{
    bytes::complete::take_while_m_n, character::is_hex_digit, error::ParseError, multi::many1,
    number::complete::hex_u32, IResult,
};

/// Parse bytes encoded as a ASCII hex string.
///
/// For example the string '1275' would result in
/// the bytes 0x12 0x75.
pub fn hex_bytes(input: &[u8]) -> IResult<&[u8], Vec<u8>> {
    let (input, bytes) = many1(hex_byte)(input)?;

    Ok((input, bytes))
}

fn hex_byte(input: &[u8]) -> IResult<&[u8], u8> {
    let (input, digits) = take_while_m_n(2, 2, is_hex_digit)(input)?;

    let result = (digits[0] as char).to_digit(16).unwrap_or(0) << 4
        | (digits[1] as char).to_digit(16).unwrap_or(0);

    Ok((input, result as u8))
}

pub fn hex_u64(input: &[u8]) -> IResult<&[u8], u64> {
    // acquire hex_bytes at first
    let (input, raw_bytes) = take_while_m_n(1, 16, is_hex_digit)(input)?;

    let mut value = 0u64;

    for digit in raw_bytes {
        value <<= 4;

        // unwrap is safe, we check above that only valid hex digits are in raw_bytes
        value |= (*digit as char).to_digit(16).unwrap() as u64;
    }

    Ok((input, value))
}

/// Parse a 32 bit number from hexadecimal characters, but hex bytes are in little endian.
pub fn hex_u32_le<'a, E: ParseError<&'a [u8]>>(input: &'a [u8]) -> IResult<&'a [u8], u32, E> {
    let (input, val) = hex_u32(input)?;

    Ok((input, val.swap_bytes()))
}

#[cfg(test)]
mod test {
    use super::*;

    const EMPTY: &[u8] = &[];

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

    #[test]
    fn parse_hex_u64() {
        assert_eq!(hex_u64(b"0").unwrap(), (EMPTY, 0x0));
        assert_eq!(hex_u64(b"00000000").unwrap(), (EMPTY, 0x0));
        assert_eq!(
            hex_u64(b"00000000000000000").unwrap(),
            ("0".as_bytes(), 0x0)
        );

        assert_eq!(
            hex_u64(b"1230000000000000").unwrap(),
            (EMPTY, 0x1230_0000_0000_0000)
        );
    }
}
