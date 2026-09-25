use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

/// Used by serde to provide defaults for `RttChannelConfig::show_timestamps`
fn default_show_timestamps() -> bool {
    true
}

#[derive(
    Debug, Copy, Clone, PartialEq, Eq, Default, docsplay::Display, Serialize, Deserialize, Schema,
)]
pub enum DataFormat {
    #[default]
    /// string
    String,
    /// binary
    BinaryLE,
    /// defmt
    Defmt,
}

/// Specifies what to do when a channel doesn't have enough buffer space for a complete write on the
/// target side.
#[derive(Clone, Copy, Eq, PartialEq, Debug, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[repr(u32)]
pub enum ChannelMode {
    /// Skip writing the data completely if it doesn't fit in its entirety.
    NoBlockSkip = 0,

    /// Write as much as possible of the data and ignore the rest.
    NoBlockTrim = 1,

    /// Block (spin) if the buffer is full. Note that if the application writes within a critical
    /// section, using this mode can cause the application to freeze if the buffer becomes full and
    /// is not read by the host.
    BlockIfFull = 2,
}

/// The User specified configuration for each active RTT Channel.
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct RttChannelConfig {
    pub channel_number: Option<u32>,
    #[serde(default)]
    pub data_format: DataFormat,

    /// RTT channel operating mode. Defaults to the target's configuration.
    #[serde(default)]
    pub mode: Option<ChannelMode>,

    #[serde(default = "default_show_timestamps")]
    /// Controls the inclusion of timestamps for [`DataFormat::String`] and [`DataFormat::Defmt`].
    pub show_timestamps: bool,

    #[serde(default)]
    /// Controls the inclusion of source location information for DataFormat::Defmt.
    pub show_location: bool,

    #[serde(default)]
    /// Controls the output format for DataFormat::Defmt.
    pub log_format: Option<String>,
}

impl Default for RttChannelConfig {
    fn default() -> Self {
        RttChannelConfig {
            channel_number: Default::default(),
            data_format: Default::default(),
            mode: Default::default(),
            show_timestamps: default_show_timestamps(),
            show_location: Default::default(),
            log_format: Default::default(),
        }
    }
}
