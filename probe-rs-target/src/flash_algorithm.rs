use super::flash_properties::FlashProperties;

use serde::{Deserialize, Serialize};

/// The raw flash algorithm is the description of a flash algorithm,
/// and is usually read from a target description file.
///
/// Before it can be used for flashing, it has to be assembled for
/// a specific chip, using the [RawFlashAlgorithm::assemble] function. This function
/// will determine the RAM addresses which are used when flashing.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RawFlashAlgorithm {
    /// The name of the flash algorithm.
    pub name: String,
    /// The description of the algorithm.
    pub description: String,
    /// Whether this flash algorithm is the default one or not.
    pub default: bool,
    /// List of 32-bit words containing the position-independent code for the algo.
    #[serde(deserialize_with = "deserialize")]
    #[serde(serialize_with = "serialize")]
    pub instructions: Vec<u8>,
    /// Address of the `Init()` entry point. Optional.
    pub pc_init: Option<u32>,
    /// Address of the `UnInit()` entry point. Optional.
    pub pc_uninit: Option<u32>,
    /// Address of the `ProgramPage()` entry point.
    pub pc_program_page: u32,
    /// Address of the `EraseSector()` entry point.
    pub pc_erase_sector: u32,
    /// Address of the `EraseAll()` entry point. Optional.
    pub pc_erase_all: Option<u32>,
    /// The offset from the start of RAM to the data section.
    pub data_section_offset: u32,
    /// The properties of the flash on the device.
    pub flash_properties: FlashProperties,
}

pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&base64::encode(bytes))
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
            base64::decode(v).map_err(serde::de::Error::custom)
        }
    }

    deserializer.deserialize_str(Base64Visitor)
}
