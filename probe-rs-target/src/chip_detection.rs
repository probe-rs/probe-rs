//! Chip detection information.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Vendor-specific chip detection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChipDetectionMethod {
    /// Microchip ATSAM chip detection information.
    AtsamDsu(AtsamDsuDetection),

    /// Espressif chip detection information.
    Espressif(EspressifDetection),

    /// Nordic Semiconductor FICR CONFIGID-based chip detection information.
    NordicConfigId(NordicConfigIdDetection),

    /// Nordic Semiconductor FICR INFO-based chip detection information.
    NordicFicrInfo(NordicFicrDetection),
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
}

/// Microchip ATSAM chip detection information when the device contains a DSU.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtsamDsuDetection {
    /// DSU DID register, Processor field
    pub processor: u8,

    /// DSU DID register, Family field
    pub family: u8,

    /// DSU DID register, Series field
    pub series: u8,

    /// Devsel => Target field value
    pub variants: HashMap<u8, String>,
}

/// Espressif chip detection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EspressifDetection {
    /// Debug module IDCODE
    pub idcode: u32,

    /// Magic chip value => Target name.
    pub variants: HashMap<u32, String>,
}

/// Nordic FICR CONFIGID-based chip detection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NordicConfigIdDetection {
    /// FICR CONFIGID address
    pub configid_address: u32,

    /// CONFIGID.HWID => Target name.
    pub hwid: HashMap<u32, String>,
}

/// Nordic FICR INFO-based chip detection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NordicFicrDetection {
    /// FICR INFO.PART address
    pub part_address: u32,

    /// FICR INFO.VARIANT address
    pub variant_address: u32,

    /// The value of INFO.PART
    pub part: u32,

    /// INFO.VARIANT => Target name.
    pub variants: HashMap<u32, String>,
}
