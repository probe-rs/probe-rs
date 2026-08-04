#[cfg(feature = "clap")]
use std::num::ParseIntError;

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

#[cfg(feature = "clap")]
fn parse_u32(input: &str) -> Result<u32, ParseIntError> {
    parse_int::parse(input)
}

#[cfg(feature = "clap")]
fn parse_u64(input: &str) -> Result<u64, ParseIntError> {
    parse_int::parse(input)
}

#[derive(Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[cfg_attr(feature = "clap", derive(clap::Parser))]
#[serde(default)]
pub struct BinaryCliOptions {
    #[cfg_attr(
        feature = "clap",
        clap(
            long,
            value_parser = parse_u64,
            help_heading = "DOWNLOAD CONFIGURATION / BIN IMAGE"
        )
    )]
    pub base_address: Option<u64>,
    #[cfg_attr(
        feature = "clap",
        clap(
            long,
            value_parser = parse_u32,
            default_value = "0",
            help_heading = "DOWNLOAD CONFIGURATION / BIN IMAGE"
        )
    )]
    pub skip: u32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "lowercase")]
pub enum EspFlashFrequency {
    #[serde(rename = "12MHz")]
    _12Mhz,
    #[serde(rename = "15MHz")]
    _15Mhz,
    #[serde(rename = "16MHz")]
    _16Mhz,
    #[serde(rename = "20MHz")]
    _20Mhz,
    #[serde(rename = "24MHz")]
    _24Mhz,
    #[serde(rename = "26MHz")]
    _26Mhz,
    #[serde(rename = "30MHz")]
    _30Mhz,
    #[serde(rename = "40MHz")]
    #[default]
    _40Mhz,
    #[serde(rename = "48MHz")]
    _48Mhz,
    #[serde(rename = "60MHz")]
    _60Mhz,
    #[serde(rename = "80MHz")]
    _80Mhz,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "lowercase")]
pub enum EspFlashMode {
    Qio,
    Qout,
    #[default]
    Dio,
    Dout,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[cfg_attr(feature = "clap", derive(clap::Parser))]
#[serde(default)]
pub struct IdfCliOptions {
    #[cfg_attr(
        feature = "clap",
        clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")
    )]
    pub idf_bootloader: Option<String>,
    #[cfg_attr(
        feature = "clap",
        clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")
    )]
    pub idf_partition_table: Option<String>,
    #[cfg_attr(
        feature = "clap",
        clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")
    )]
    pub idf_target_app_partition: Option<String>,
    #[cfg_attr(
        feature = "clap",
        clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")
    )]
    pub idf_flash_mode: Option<EspFlashMode>,
    #[cfg_attr(
        feature = "clap",
        clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")
    )]
    pub idf_flash_freq: Option<EspFlashFrequency>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[cfg_attr(feature = "clap", derive(clap::Parser))]
#[serde(default)]
pub struct ElfCliOptions {
    #[cfg_attr(
        feature = "clap",
        clap(long, help_heading = "DOWNLOAD CONFIGURATION / ELF IMAGE")
    )]
    pub skip_section: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[cfg_attr(feature = "clap", derive(clap::Parser))]
#[serde(default)]
pub struct FormatOptions {
    #[cfg_attr(
        feature = "clap",
        clap(
            value_enum,
            ignore_case = true,
            default_value_t = FormatKind::Target,
            long,
            help_heading = "DOWNLOAD CONFIGURATION"
        )
    )]
    pub binary_format: FormatKind,

    #[cfg_attr(feature = "clap", clap(flatten))]
    pub bin_options: BinaryCliOptions,

    #[cfg_attr(feature = "clap", clap(flatten))]
    pub idf_options: IdfCliOptions,

    #[cfg_attr(feature = "clap", clap(flatten))]
    pub elf_options: ElfCliOptions,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq, Clone, Copy, Schema)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum FormatKind {
    #[default]
    Target,

    #[cfg_attr(feature = "clap", value(alias("binary")))]
    Bin,

    #[cfg_attr(feature = "clap", value(aliases(["ihex", "intelhex"])))]
    Hex,

    Elf,

    #[cfg_attr(feature = "clap", value(aliases(["esp-idf", "espidf"])))]
    Idf,

    Uf2,
}

impl FormatKind {
    pub fn from_optional(s: Option<&str>) -> Result<Self, String> {
        match s {
            Some(format) => match format.to_ascii_lowercase().as_str() {
                "target" => Ok(Self::Target),
                "bin" | "binary" => Ok(Self::Bin),
                "hex" | "ihex" | "intelhex" => Ok(Self::Hex),
                "elf" => Ok(Self::Elf),
                "idf" | "esp-idf" | "espidf" => Ok(Self::Idf),
                "uf2" => Ok(Self::Uf2),
                _ => Err(format!("invalid variant: {format}")),
            },
            None => Ok(Self::Elf),
        }
    }

    pub fn resolve_default_format(self, default_format: Option<&str>) -> FormatKind {
        if self == FormatKind::Target {
            FormatKind::from_optional(default_format)
                .expect("Failed to parse a default binary format. This shouldn't happen.")
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FormatKind;

    #[test]
    fn format_kind_resolve_default_format_uses_server_hint() {
        assert_eq!(
            FormatKind::Target.resolve_default_format(Some("idf")),
            FormatKind::Idf
        );
        assert_eq!(
            FormatKind::Target.resolve_default_format(None),
            FormatKind::Elf
        );
        assert_eq!(
            FormatKind::Bin.resolve_default_format(Some("elf")),
            FormatKind::Bin
        );
    }
}
