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

use std::{num::ParseIntError, sync::LazyLock};

/// The `--version` long string, extended with the defmt wire formats this build can decode.
///
/// A firmware built against a defmt version this decoder does not understand fails with an
/// opaque serde error, so surfacing the supported formats here gives users something concrete
/// to compare their firmware against.
pub fn long_version() -> &'static str {
    static LONG_VERSION: LazyLock<String> = LazyLock::new(|| {
        format!(
            "{}\ndefmt wire formats: {}",
            env!("PROBE_RS_LONG_VERSION"),
            defmt_decoder::DEFMT_VERSIONS.join(", ")
        )
    });

    &LONG_VERSION
}

pub fn parse_u32(input: &str) -> Result<u32, ParseIntError> {
    parse_int::parse(input)
}

pub fn parse_u64(input: &str) -> Result<u64, ParseIntError> {
    parse_int::parse(input)
}
