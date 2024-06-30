use super::flash_properties::FlashProperties;
use crate::serialize::{hex_option, hex_u_int};
use base64::{engine::general_purpose as base64_engine, Engine as _};
use serde::{Deserialize, Serialize};

/// Data encoding used by the flash algorithm.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TransferEncoding {
    /// Raw binary encoding. Probe-rs will not apply any transformation to the flash data.
    #[default]
    Raw,

    /// Flash data is compressed using the `miniz_oxide` crate.
    ///
    /// Compressed images are written in page sized chunks, each chunk written to the image's start
    /// address. The length of the compressed image is stored in the first 4 bytes of the first
    /// chunk of the image.
    Miniz,
}

/// The raw flash algorithm is the description of a flash algorithm,
/// and is usually read from a target description file.
///
/// Before it can be used for flashing, it has to be assembled for
/// a specific chip, by determining the RAM addresses which are used when flashing.
/// This process is done in the main `probe-rs` library.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct RawFlashAlgorithm {
    /// The name of the flash algorithm.
    pub name: String,
    /// The description of the algorithm.
    pub description: String,
    /// Whether this flash algorithm is the default one or not.
    #[serde(default)]
    pub default: bool,
    /// List of 32-bit words containing the code for the algo. If `load_address` is not specified, the code must be position independent (PIC).
    #[serde(deserialize_with = "deserialize")]
    #[serde(serialize_with = "serialize")]
    pub instructions: Vec<u8>,
    /// Address to load algo into RAM. Optional.
    #[serde(serialize_with = "hex_option")]
    pub load_address: Option<u64>,
    /// Address to load data into RAM. Optional.
    #[serde(serialize_with = "hex_option")]
    pub data_load_address: Option<u64>,
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
    /// Location of the RTT control block in RAM.
    ///
    /// If this is set, the flash algorithm supports RTT output
    /// and debug messages will be read over RTT.
    #[serde(serialize_with = "hex_option")]
    pub rtt_location: Option<u64>,
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

    /// The encoding format accepted by the flash algorithm.
    #[serde(default)]
    pub transfer_encoding: Option<TransferEncoding>,
}

pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    // Use a separate, more compact representation for binary formats.
    if serializer.is_human_readable() {
        Base64::serialize(bytes, serializer)
    } else {
        Bytes::serialize(bytes, serializer)
    }
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Use a separate, more compact representation for binary formats.
    if deserializer.is_human_readable() {
        Base64::deserialize(deserializer)
    } else {
        Bytes::deserialize(deserializer)
    }
}

struct Base64;
impl Base64 {
    fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(base64_engine::STANDARD.encode(bytes).as_str())
    }

    fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(Base64)
    }
}
impl<'de> serde::de::Visitor<'de> for Base64 {
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

struct Bytes;
impl Bytes {
    fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_bytes(bytes)
    }

    fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_bytes(Bytes)
    }
}
impl<'de> serde::de::Visitor<'de> for Bytes {
    type Value = Vec<u8>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "binary data")
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(v.to_vec())
    }
}
