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

/// Long version string for `--version`, listing the defmt wire formats this build can decode.
///
/// Shared by the `probe-rs`, `cargo embed` and `cargo flash` clap definitions. Firmware built
/// against a defmt version we cannot read fails deep inside the decoder with an opaque serde
/// error (see [`rtt::processing`]), so naming the formats we do support gives users something
/// concrete to compare against.
///
/// Only the *long* version grows: [`meta::current_meta`] parses `PROBE_RS_VERSION` as semver,
/// so that one has to stay a bare version number.
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
