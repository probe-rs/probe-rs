pub mod cargo;
pub mod cli;
pub mod common_options;
pub mod flash;
pub mod logging;
pub mod meta;
pub mod rtt;
pub mod visualizer;

use std::num::ParseIntError;
use std::ops::Range;

#[derive(thiserror::Error, docsplay::Display, Clone, Debug, Eq, PartialEq)]
pub enum ParseRangeError {
    /// Range separator '..' not found
    MissingSeparator,
    /// Parsing range bounds failed ({0})
    InvalidBound(#[from] ParseIntError),
}

/// Parses an exclusive range in the form `START..END` for `u64` bounds.
pub fn parse_range_exclusive_u64(input: &str) -> Result<Range<u64>, ParseRangeError> {
    // The parse_int crate started to add support for parsing ranges. But there is currently an
    // issue with parsing exclusive ranges
    // (https://gitlab.com/dns2utf8/parse_int/-/merge_requests/2) which prevents us from using
    // range parsing and it looks like the fix is not going to land in the next days. So we're
    // shipping our own range parsing for now.
    if let Some((start, end)) = input.split_once("..") {
        let start = start.trim();
        let end = end.trim();

        match (parse_u64(start), parse_u64(end)) {
            (Ok(start), Ok(end)) => Ok(start..end),
            (Err(e), _) | (_, Err(e)) => Err(ParseRangeError::InvalidBound(e)),
        }
    } else {
        Err(ParseRangeError::MissingSeparator)
    }
}

pub fn parse_u32(input: &str) -> Result<u32, ParseIntError> {
    parse_int::parse(input)
}

pub fn parse_u64(input: &str) -> Result<u64, ParseIntError> {
    parse_int::parse(input)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parse_range_simple_invalid_input() {
        assert!(parse_range_exclusive_u64("").is_err());
        assert!(parse_range_exclusive_u64("    ").is_err());
        assert!(parse_range_exclusive_u64("42").is_err());
        assert!(parse_range_exclusive_u64("..").is_err());
        assert!(parse_range_exclusive_u64("....").is_err());
    }

    #[test]
    fn parse_range_invalid_separator() {
        assert_eq!(
            parse_range_exclusive_u64("1 4"),
            Err(ParseRangeError::MissingSeparator)
        );
        assert_eq!(
            parse_range_exclusive_u64("1-4"),
            Err(ParseRangeError::MissingSeparator)
        );
        assert_eq!(
            parse_range_exclusive_u64("1:4"),
            Err(ParseRangeError::MissingSeparator)
        );
        // The inclusive range separator contains the exclusive range separator and therefor the
        // equals sign is considered part of the upper bound. Let's keep a low profile and leave
        // this corner case as-is.
        assert!(matches!(
            parse_range_exclusive_u64("1..=3"),
            Err(ParseRangeError::InvalidBound(_))
        ));
    }

    #[test]
    fn parse_range_invalid_bound() {
        assert!(matches!(
            parse_range_exclusive_u64("..4"),
            Err(ParseRangeError::InvalidBound(_))
        ));
        assert!(matches!(
            parse_range_exclusive_u64("1.."),
            Err(ParseRangeError::InvalidBound(_))
        ));
        assert!(matches!(
            parse_range_exclusive_u64(".."),
            Err(ParseRangeError::InvalidBound(_))
        ));

        assert!(matches!(
            parse_range_exclusive_u64("x..4"),
            Err(ParseRangeError::InvalidBound(_))
        ));
        assert!(matches!(
            parse_range_exclusive_u64("x.."),
            Err(ParseRangeError::InvalidBound(_))
        ));
        assert!(matches!(
            parse_range_exclusive_u64("..y"),
            Err(ParseRangeError::InvalidBound(_))
        ));
        assert!(matches!(
            parse_range_exclusive_u64("1..y"),
            Err(ParseRangeError::InvalidBound(_))
        ));
    }

    #[test]
    fn parse_range_valid_bounds_and_separator() {
        // Some valid non-empty ranges with decimal bounds.
        assert_eq!(parse_range_exclusive_u64("0..1"), Ok(0..1));
        assert_eq!(parse_range_exclusive_u64("0..2"), Ok(0..2));
        assert_eq!(
            parse_range_exclusive_u64("0..18446744073709551615"),
            Ok(0..u64::MAX)
        );
        assert_eq!(
            parse_range_exclusive_u64("18446744073709551614..18446744073709551615"),
            Ok((u64::MAX - 1)..u64::MAX)
        );

        // Empty ranges are allowed.
        assert_eq!(parse_range_exclusive_u64("0..0"), Ok(0..0));
        assert_eq!(parse_range_exclusive_u64("1..0"), Ok(1..0));
        assert_eq!(
            parse_range_exclusive_u64("18446744073709551615..0"),
            Ok(u64::MAX..0)
        );

        // Bounds may be given as hexadecimal numbers.
        assert_eq!(parse_range_exclusive_u64("0x0..0x1"), Ok(0x0..0x1));
        assert_eq!(
            parse_range_exclusive_u64("0x1234..0x567890abcdef"),
            Ok(0x1234..0x567890abcdef)
        );
        assert_eq!(
            parse_range_exclusive_u64("0x0..0xffffffffffffffff"),
            Ok(0x0..u64::MAX)
        );

        // Bounds may be given in mixed numeral systems.
        assert_eq!(parse_range_exclusive_u64("1..0x2"), Ok(1..2));
        assert_eq!(parse_range_exclusive_u64("0x1..2"), Ok(1..2));

        // Bounds may be surrounded by whitespaces.
        assert_eq!(parse_range_exclusive_u64("1 .. 2"), Ok(1..2));
        assert_eq!(parse_range_exclusive_u64("1.. 2"), Ok(1..2));
        assert_eq!(parse_range_exclusive_u64("1 ..2"), Ok(1..2));
        assert_eq!(parse_range_exclusive_u64(" 1 .. 2 "), Ok(1..2));
        assert_eq!(parse_range_exclusive_u64(" 1.. 2 "), Ok(1..2));
        assert_eq!(parse_range_exclusive_u64(" 1 ..2 "), Ok(1..2));
    }
}
