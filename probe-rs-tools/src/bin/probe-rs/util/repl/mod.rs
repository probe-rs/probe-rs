use std::ops::Range;

use crate::util::{ArgumentParseError, parse_u64};

pub fn parse_ranges(args: &[&str]) -> Result<Vec<Range<u64>>, ArgumentParseError> {
    args
    .chunks(2)
    .enumerate()
    .map(|(i,c)| {
        let start = if let Some(start) = c.first() {
            parse_u64(start).map_err(|e| {
                ArgumentParseError {
                    argument_index: i,
                    argument: start.to_string(),
                    source: e.into(),
                }
            })?
        } else {
            unreachable!("This should never be reached as there cannot be an odd number of arguments. Please report this as a bug.")
        };

        let size = if let Some(size) = c.get(1) {
            parse_u64(size).map_err(|e| {
                ArgumentParseError {
                    argument_index: i,
                    argument: size.to_string(),
                    source: e.into(),
                }
            })?
        } else {
            unreachable!("This should never be reached as there cannot be an odd number of arguments. Please report this as a bug.")
        };

        Ok::<_, ArgumentParseError>(start..start + size)
    })
    .collect::<Result<Vec<Range<u64>>, _>>()
}
