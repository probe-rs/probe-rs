pub mod cargo;
pub mod cli;
pub mod common_options;
pub mod flash;
pub mod logging;
pub mod meta;
pub mod pwr;
pub mod rtt;
pub mod setup_hints;
pub mod visualizer;

use std::num::ParseIntError;

pub fn parse_u64(input: &str) -> Result<u64, ParseIntError> {
    parse_int::parse(input)
}

pub fn parse_duration_secs(input: &str) -> Result<std::time::Duration, String> {
    let seconds = input
        .parse::<f64>()
        .map_err(|_| format!("`{input}` is not a number of seconds"))?;

    std::time::Duration::try_from_secs_f64(seconds).map_err(|error| error.to_string())
}
