use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[serde(default)]
pub struct BinaryFormatOptions {
    pub base_address: Option<u64>,
    pub skip: u32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Schema)]
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
#[serde(rename_all = "lowercase")]
pub enum EspFlashMode {
    Qio,
    Qout,
    #[default]
    Dio,
    Dout,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[serde(default)]
pub struct IdfFormatOptions {
    pub idf_bootloader: Option<String>,
    pub idf_partition_table: Option<String>,
    pub idf_target_app_partition: Option<String>,
    pub idf_flash_mode: Option<EspFlashMode>,
    pub idf_flash_freq: Option<EspFlashFrequency>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[serde(default)]
pub struct ElfFormatOptions {
    pub skip_section: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default, Schema)]
#[serde(default)]
pub struct FormatOptions {
    pub binary_format: FormatKind,

    pub bin_options: BinaryFormatOptions,

    pub idf_options: IdfFormatOptions,

    pub elf_options: ElfFormatOptions,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq, Clone, Copy, Schema)]
pub enum FormatKind {
    #[default]
    Target,

    Bin,

    Hex,

    Elf,

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
