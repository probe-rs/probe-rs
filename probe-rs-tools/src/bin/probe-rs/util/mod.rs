pub mod cargo;
pub mod cli;
pub mod common_options;
pub mod flash;
pub mod logging;
pub mod meta;
pub mod rtt;
pub mod visualizer;

use std::num::ParseIntError;

pub fn parse_u32(input: &str) -> Result<u32, ParseIntError> {
    parse_int::parse(input)
}

pub fn parse_u64(input: &str) -> Result<u64, ParseIntError> {
    parse_int::parse(input)
}

#[derive(Debug, thiserror::Error, docsplay::Display)]
#[error("Failed to parse argument '{argument}'.")]
pub struct ArgumentParseError {
    pub argument_index: usize,
    pub argument: String,
    pub source: anyhow::Error,
}
