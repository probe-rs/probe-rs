//! The image format options of the command line and of the DAP launch configuration.
//!
//! The RPC server takes [`format::FormatOptions`]. These types convert to it.

use std::num::ParseIntError;

use probe_rs_rpc::format;
use serde::{Deserialize, Serialize};

fn parse_u32(input: &str) -> Result<u32, ParseIntError> {
    parse_int::parse(input)
}

fn parse_u64(input: &str) -> Result<u64, ParseIntError> {
    parse_int::parse(input)
}

#[derive(Clone, Serialize, Deserialize, Debug, Default, clap::Parser)]
#[serde(default)]
pub struct BinaryCliOptions {
    #[clap(
            long,
            value_parser = parse_u64,
            help_heading = "DOWNLOAD CONFIGURATION / BIN IMAGE"
        )]
    pub base_address: Option<u64>,
    #[clap(
            long,
            value_parser = parse_u32,
            default_value = "0",
            help_heading = "DOWNLOAD CONFIGURATION / BIN IMAGE"
        )]
    pub skip: u32,
}

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
#[expect(
    clippy::enum_variant_names,
    reason = "The variant names are the values of the command line option"
)]
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

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum EspFlashMode {
    Qio,
    Qout,
    #[default]
    Dio,
    Dout,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default, clap::Parser)]
#[serde(default)]
pub struct IdfCliOptions {
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")]
    pub idf_bootloader: Option<String>,
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")]
    pub idf_partition_table: Option<String>,
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")]
    pub idf_target_app_partition: Option<String>,
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")]
    pub idf_flash_mode: Option<EspFlashMode>,
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION / ESP-IDF IMAGE")]
    pub idf_flash_freq: Option<EspFlashFrequency>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default, clap::Parser)]
#[serde(default)]
pub struct ElfCliOptions {
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION / ELF IMAGE")]
    pub skip_section: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default, clap::Parser)]
#[serde(default)]
pub struct FormatOptions {
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

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq, Clone, Copy, clap::ValueEnum)]
pub enum FormatKind {
    #[default]
    Target,

    #[value(alias("binary"))]
    Bin,

    #[value(aliases(["ihex", "intelhex"]))]
    Hex,

    Elf,

    #[value(aliases(["esp-idf", "espidf"]))]
    Idf,

    Uf2,
}

impl From<FormatOptions> for format::FormatOptions {
    fn from(options: FormatOptions) -> Self {
        Self {
            binary_format: options.binary_format.into(),
            bin_options: format::BinaryFormatOptions {
                base_address: options.bin_options.base_address,
                skip: options.bin_options.skip,
            },
            idf_options: format::IdfFormatOptions {
                idf_bootloader: options.idf_options.idf_bootloader,
                idf_partition_table: options.idf_options.idf_partition_table,
                idf_target_app_partition: options.idf_options.idf_target_app_partition,
                idf_flash_mode: options.idf_options.idf_flash_mode.map(Into::into),
                idf_flash_freq: options.idf_options.idf_flash_freq.map(Into::into),
            },
            elf_options: format::ElfFormatOptions {
                skip_section: options.elf_options.skip_section,
            },
        }
    }
}

impl From<FormatKind> for format::FormatKind {
    fn from(kind: FormatKind) -> Self {
        match kind {
            FormatKind::Target => Self::Target,
            FormatKind::Bin => Self::Bin,
            FormatKind::Hex => Self::Hex,
            FormatKind::Elf => Self::Elf,
            FormatKind::Idf => Self::Idf,
            FormatKind::Uf2 => Self::Uf2,
        }
    }
}

impl From<EspFlashFrequency> for format::EspFlashFrequency {
    fn from(frequency: EspFlashFrequency) -> Self {
        match frequency {
            EspFlashFrequency::_12Mhz => Self::_12Mhz,
            EspFlashFrequency::_15Mhz => Self::_15Mhz,
            EspFlashFrequency::_16Mhz => Self::_16Mhz,
            EspFlashFrequency::_20Mhz => Self::_20Mhz,
            EspFlashFrequency::_24Mhz => Self::_24Mhz,
            EspFlashFrequency::_26Mhz => Self::_26Mhz,
            EspFlashFrequency::_30Mhz => Self::_30Mhz,
            EspFlashFrequency::_40Mhz => Self::_40Mhz,
            EspFlashFrequency::_48Mhz => Self::_48Mhz,
            EspFlashFrequency::_60Mhz => Self::_60Mhz,
            EspFlashFrequency::_80Mhz => Self::_80Mhz,
        }
    }
}

impl From<EspFlashMode> for format::EspFlashMode {
    fn from(mode: EspFlashMode) -> Self {
        match mode {
            EspFlashMode::Qio => Self::Qio,
            EspFlashMode::Qout => Self::Qout,
            EspFlashMode::Dio => Self::Dio,
            EspFlashMode::Dout => Self::Dout,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DAP launch configuration reads the same options as the RPC types did before.
    #[test]
    fn launch_configuration_reads_the_same_options() {
        let json = serde_json::json!({
            "binary_format": "Idf",
            "bin_options": { "base_address": 4096, "skip": 16 },
            "idf_options": {
                "idf_bootloader": "boot.bin",
                "idf_partition_table": "parts.csv",
                "idf_target_app_partition": "factory",
                "idf_flash_mode": "qout",
                "idf_flash_freq": "80MHz"
            },
            "elf_options": { "skip_section": [".bss"] }
        });

        let cli: FormatOptions = serde_json::from_value(json.clone()).unwrap();
        let wire: format::FormatOptions = serde_json::from_value(json).unwrap();

        assert_eq!(
            serde_json::to_value(format::FormatOptions::from(cli)).unwrap(),
            serde_json::to_value(wire).unwrap()
        );
    }
}
