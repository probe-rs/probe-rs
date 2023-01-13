use super::flash_properties::FlashProperties;
use crate::serialize::{hex_option, hex_u_int};
use base64::{engine::general_purpose as base64_engine, Engine as _};
use serde::{Deserialize, Serialize};

/// The raw flash algorithm is the description of a flash algorithm,
/// and is usually read from a target description file.
///
/// Before it can be used for flashing, it has to be assembled for
/// a specific chip, by determining the RAM addresses which are used when flashing.
/// This process is done in the main `probe-rs` library.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RawFlashAlgorithm {
    /// The name of the flash algorithm.
    pub name: String,
    /// The description of the algorithm.
    pub description: String,
    /// Whether this flash algorithm is the default one or not.
    #[serde(default)]
    pub default: bool,
    /// List of 32-bit words containing the code for the algo. If `load_address` is not specified, the code must be position indepent (PIC).
    #[serde(deserialize_with = "deserialize")]
    #[serde(serialize_with = "serialize")]
    pub instructions: Vec<u8>,
    /// Address to load algo into RAM. Optional.
    #[serde(serialize_with = "hex_option")]
    pub load_address: Option<u64>,
    /// Address of the `Init()` entry point. Optional.
    #[serde(serialize_with = "hex_option")]
    pub pc_init: Option<u64>,
    /// Address of the `UnInit()` entry point. Optional.
    #[serde(serialize_with = "hex_option")]
    pub pc_uninit: Option<u64>,
    /// Address of the `ProgramPage()` entry point.
    #[serde(serialize_with = "hex_u_int")]
    pub pc_program_page: u64,
    /// Address of the `EraseSector()` entry point.
    #[serde(serialize_with = "hex_u_int")]
    pub pc_erase_sector: u64,
    /// Address of the `EraseAll()` entry point. Optional.
    #[serde(serialize_with = "hex_option")]
    pub pc_erase_all: Option<u64>,
    /// The offset from the start of RAM to the data section.
    #[serde(serialize_with = "hex_u_int")]
    pub data_section_offset: u64,
    /// The properties of the flash on the device.
    pub flash_properties: FlashProperties,
    /// List of cores that can use this algorithm
    #[serde(default)]
    pub cores: Vec<String>,
    /// The flash algorithm's stack size, in bytes.
    ///
    /// If not set, probe-rs selects a default value.
    /// Increase this value if you're concerned about stack
    /// overruns during flashing.
    pub stack_size: Option<u32>,
}

pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(base64_engine::STANDARD.encode(bytes).as_str())
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Base64Visitor;

    impl<'de> serde::de::Visitor<'de> for Base64Visitor {
        type Value = Vec<u8>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(formatter, "base64 ASCII text")
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            base64_engine::STANDARD
                .decode(v)
                .map_err(serde::de::Error::custom)
        }
    }

    deserializer.deserialize_str(Base64Visitor)
}
