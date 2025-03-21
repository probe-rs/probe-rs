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

pub fn dumped_ranges_to_string(ranges: &[Range<u64>]) -> String {
    let mut range_string = String::new();
    let mut first = true;
    for memory_range in ranges {
        if !first {
            range_string.push_str(", ");
        }
        first = false;
        range_string.push_str(&format!("{memory_range:#X?}"));
    }
    if range_string.is_empty() {
        range_string = "(No memory ranges specified)".to_string();
    } else {
        range_string = format!("(Includes memory ranges: {range_string})");
    }

    range_string
}
