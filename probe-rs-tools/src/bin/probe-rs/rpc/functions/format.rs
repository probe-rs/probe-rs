use std::num::ParseIntError;

use clap::ValueEnum;
use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

fn parse_u32(input: &str) -> Result<u32, ParseIntError> {
    parse_int::parse(input)
}

fn parse_u64(input: &str) -> Result<u64, ParseIntError> {
    parse_int::parse(input)
}

#[derive(clap::Parser, Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[serde(default)]
pub struct BinaryCliOptions {
    /// The address in memory where the binary will be put at. This is only considered when `bin` is selected as the format.
    #[clap(long, value_parser = parse_u64, help_heading = "DOWNLOAD CONFIGURATION / BIN IMAGE")]
    pub base_address: Option<u64>,
    /// The number of bytes to skip at the start of the binary file. This is only considered when `bin` is selected as the format.
    #[clap(long, value_parser = parse_u32, default_value = "0", help_heading = "DOWNLOAD CONFIGURATION / BIN IMAGE")]
    pub skip: u32,
}

/// Supported flash frequencies
///
/// Note that not all frequencies are supported by each target device.
#[expect(clippy::enum_variant_names)]
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum, Schema,
)]
#[serde(rename_all = "lowercase")]
pub enum EspFlashFrequency {
    /// 12 MHz
    #[serde(rename = "12MHz")]
    _12Mhz,
    /// 15 MHz
    #[serde(rename = "15MHz")]
    _15Mhz,
    /// 16 MHz
    #[serde(rename = "16MHz")]
    _16Mhz,
    /// 20 MHz
    #[serde(rename = "20MHz")]
    _20Mhz,
    /// 24 MHz
    #[serde(rename = "24MHz")]
    _24Mhz,
    /// 26 MHz
    #[serde(rename = "26MHz")]
    _26Mhz,
    /// 30 MHz
    #[serde(rename = "30MHz")]
    _30Mhz,
    /// 40 MHz
    #[serde(rename = "40MHz")]
    #[default]
    _40Mhz,
    /// 48 MHz
    #[serde(rename = "48MHz")]
    _48Mhz,
    /// 60 MHz
    #[serde(rename = "60MHz")]
    _60Mhz,
    /// 80 MHz
    #[serde(rename = "80MHz")]
    _80Mhz,
}

/// Supported flash modes
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum, Schema,
)]
#[serde(rename_all = "lowercase")]
pub enum EspFlashMode {
    /// Quad I/O (4 pins used for address & data)
    Qio,
    /// Quad Output (4 pins used for data)
    Qout,
    /// Dual I/O (2 pins used for address & data)
    #[default]
    Dio,
    /// Dual Output (2 pins used for data)
    Dout,
}

#[derive(clap::Parser, Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[serde(default)]
pub struct IdfCliOptions {
    /// The idf bootloader path
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")]
    pub idf_bootloader: Option<String>,
    /// The idf partition table path
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")]
    pub idf_partition_table: Option<String>,
    /// The idf target app partition
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")]
    pub idf_target_app_partition: Option<String>,
    /// Flash SPI mode
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")]
    pub idf_flash_mode: Option<EspFlashMode>,
    /// Flash SPI frequency
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")]
    pub idf_flash_freq: Option<EspFlashFrequency>,
}

#[derive(clap::Parser, Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[serde(default)]
pub struct ElfCliOptions {
    /// Section name to skip flashing. This option may be specified multiple times, and is only
    /// considered when `elf` is selected as the format.
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION / ELF IMAGE")]
    pub skip_section: Vec<String>,
}

#[derive(clap::Parser, Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[serde(default)]
pub struct FormatOptions {
    /// The format of the firmware image.
    #[clap(
        value_enum,
        ignore_case = true,
        default_value_t = FormatKind::Target,
        long,
        help_heading = "DOWNLOAD CONFIGURATION"
    )]
    pub binary_format: FormatKind,

    #[clap(flatten)]
    pub bin_options: BinaryCliOptions,

    #[clap(flatten)]
    pub idf_options: IdfCliOptions,

    #[clap(flatten)]
    pub elf_options: ElfCliOptions,
}

/// A finite list of all the available binary formats probe-rs understands.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq, Clone, Copy, ValueEnum, Schema)]
pub enum FormatKind {
    /// The image format is determined by the target chip's preference, which is usually ELF.
    #[default]
    Target,

    /// The image is in binary format. This means that the file contains the contents of the flash 1:1.
    #[value(alias("binary"))]
    Bin,

    /// The image is in Intel HEX format. For more information, see https://en.wikipedia.org/wiki/Intel_HEX
    #[value(aliases(["ihex", "intelhex"]))]
    Hex,

    /// The image is in the Executable and Linkable Format (ELF). For more information, see https://en.wikipedia.org/wiki/Executable_and_Linkable_Format
    Elf,

    /// The image is an ELF file containing an ESP-IDF bootloader compatible application. For more information, see https://docs.espressif.com/projects/esp-idf/en/latest/esp32/api-reference/system/app_image_format.html#app-image-structures
    #[value(aliases(["esp-idf", "espidf"]))]
    Idf,

    /// The image is in the Universal Flash Storage (UF2) format. For more information, see https://github.com/microsoft/uf2
    Uf2,
}

impl FormatKind {
    /// Creates a new Format from an optional string.
    ///
    /// If the string is `None`, the default format is returned.
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

    /// Replaces `FormatKind::Target` using a server-provided default format string.
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
