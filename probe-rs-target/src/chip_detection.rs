//! Chip detection information.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Vendor-specific chip detection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChipDetectionMethod {
    /// Microchip ATSAM chip detection information.
    Atsam(AtsamDetection),

    /// Espressif chip detection information.
    Espressif(EspressifDetection),
}

impl ChipDetectionMethod {
    /// Returns the ATSAM detection information if available.
    pub fn as_atsam(&self) -> Option<&AtsamDetection> {
        if let Self::Atsam(v) = self {
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
}

/// Microchip ATSAM chip detection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtsamDetection {
    /// DSU DID register, Processor field
    pub processor: u8,

    /// DSU DID register, Family field
    pub family: u8,

    /// DSU DID register, Series field
    pub series: u8,

    /// Target => Devsel field value
    pub variants: HashMap<String, u8>,
}

/// Espressif chip detection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EspressifDetection {
    /// Debug module IDCODE
    pub idcode: u32,

    /// Magic chip value => Target name.
    pub variants: HashMap<u32, String>,
}
