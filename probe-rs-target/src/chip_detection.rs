//! Chip detection information.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_with::rust::maps_duplicate_key_is_error;

use crate::serialize::{hex_keys_indexmap, hex_u_int};

/// Vendor-specific chip detection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ChipDetectionMethod {
    /// Microchip ATSAM chip detection information.
    AtsamDsu(AtsamDsuDetection),

    /// Espressif chip detection information.
    Espressif(EspressifDetection),

    /// Nordic Semiconductor FICR CONFIGID-based chip detection information.
    NordicConfigId(NordicConfigIdDetection),

    /// Nordic Semiconductor FICR INFO-based chip detection information.
    NordicFicrInfo(NordicFicrDetection),

    /// Infineon XMC4000 SCU chip detection information.
    InfineonXmcScu(InfineonXmcScuDetection),

    /// Infineon PSOC silicon ID chip detection information.
    InfineonPsocSiid(InfineonPsocSiidDetection),

    /// Renesas RA chip detection information.
    RenesasPnr(RenesasPnrDetection),
}

impl ChipDetectionMethod {
    /// Returns the ATSAM detection information if available.
    pub fn as_atsam_dsu(&self) -> Option<&AtsamDsuDetection> {
        if let Self::AtsamDsu(v) = self {
            Some(v)
        } else {
            None
        }
    }

    /// Returns the Espressif detection information if available.
    pub fn as_espressif(&self) -> Option<&EspressifDetection> {
        if let Self::Espressif(v) = self {
            Some(v)
        } else {
            None
        }
    }

    /// Returns the Nordic CONFIGID detection information if available.
    pub fn as_nordic_configid(&self) -> Option<&NordicConfigIdDetection> {
        if let Self::NordicConfigId(v) = self {
            Some(v)
        } else {
            None
        }
    }

    /// Returns the Nordic FICR detection information if available.
    pub fn as_nordic_ficr(&self) -> Option<&NordicFicrDetection> {
        if let Self::NordicFicrInfo(v) = self {
            Some(v)
        } else {
            None
        }
    }

    /// Returns the Infineon XMC SCU detection information if available.
    pub fn as_infineon_xmc_scu(&self) -> Option<&InfineonXmcScuDetection> {
        if let Self::InfineonXmcScu(v) = self {
            Some(v)
        } else {
            None
        }
    }

    /// Returns the Infineon PSOC silicon ID detection information if available.
    pub fn as_infineon_psoc_siid(&self) -> Option<&InfineonPsocSiidDetection> {
        if let Self::InfineonPsocSiid(v) = self {
            Some(v)
        } else {
            None
        }
    }

    /// Returns the Renesas detection information if available.
    pub fn as_renesas_pnr(&self) -> Option<&RenesasPnrDetection> {
        if let Self::RenesasPnr(v) = self {
            Some(v)
        } else {
            None
        }
    }
}

/// Microchip ATSAM chip detection information when the device contains a DSU.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AtsamDsuDetection {
    /// DSU DID register, Processor field
    pub processor: u8,

    /// DSU DID register, Family field
    pub family: u8,

    /// DSU DID register, Series field
    pub series: u8,

    /// Devsel => Target field value
    #[serde(serialize_with = "hex_keys_indexmap")]
    #[serde(deserialize_with = "maps_duplicate_key_is_error::deserialize")]
    pub variants: IndexMap<u8, String>,
}

/// Espressif chip detection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EspressifDetection {
    /// Debug module IDCODE
    #[serde(serialize_with = "hex_u_int")]
    pub idcode: u32,

    /// Magic chip value => Target name.
    #[serde(serialize_with = "hex_keys_indexmap")]
    #[serde(deserialize_with = "maps_duplicate_key_is_error::deserialize")]
    pub variants: IndexMap<u32, String>,
}

/// Nordic FICR CONFIGID-based chip detection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NordicConfigIdDetection {
    /// FICR CONFIGID address
    #[serde(serialize_with = "hex_u_int")]
    pub configid_address: u32,

    /// CONFIGID.HWID => Target name.
    #[serde(serialize_with = "hex_keys_indexmap")]
    #[serde(deserialize_with = "maps_duplicate_key_is_error::deserialize")]
    pub hwid: IndexMap<u32, String>,
}

/// Nordic FICR INFO-based chip detection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NordicFicrDetection {
    /// FICR INFO.PART address
    #[serde(serialize_with = "hex_u_int")]
    pub part_address: u32,

    /// FICR INFO.VARIANT address
    #[serde(serialize_with = "hex_u_int")]
    pub variant_address: u32,

    /// The value of INFO.PART
    #[serde(serialize_with = "hex_u_int")]
    pub part: u32,

    /// INFO.VARIANT => Target name.
    #[serde(serialize_with = "hex_keys_indexmap")]
    #[serde(deserialize_with = "maps_duplicate_key_is_error::deserialize")]
    pub variants: IndexMap<u32, String>,
}

/// Infineon XMC4000 SCU chip detection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfineonXmcScuDetection {
    /// Chip partid
    #[serde(serialize_with = "hex_u_int")]
    pub part: u16,

    /// SCU_IDCHIP register value, bits \[19:4\]
    #[serde(serialize_with = "hex_u_int")]
    pub scu_id: u32,

    /// Flash size in kB => Target name.
    // Intentionally not hex-keyed, sizes look better in decimal.
    #[serde(deserialize_with = "maps_duplicate_key_is_error::deserialize")]
    pub variants: IndexMap<u32, String>,
}

/// Infineon PSOC SIID chip detection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfineonPsocSiidDetection {
    /// Chip family ID
    #[serde(serialize_with = "hex_u_int")]
    pub family_id: u16,

    /// Silicon ID => Target name.
    #[serde(serialize_with = "hex_keys_indexmap")]
    #[serde(deserialize_with = "maps_duplicate_key_is_error::deserialize")]
    pub silicon_ids: IndexMap<u16, String>,
}

/// Renesas RA chip detection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenesasPnrDetection {
    /// Part number from `TARGETID`
    pub target_id: u16,

    /// `true` if the part number is stored with the last character at the lowest address.
    #[serde(default)]
    pub reverse_string: bool,

    /// Location of the first MCU part number register
    /// <https://en-support.renesas.com/knowledgeBase/21397541>
    pub mcu_pn_base: u32,

    /// Chip part number
    pub variants: Vec<String>,
}
