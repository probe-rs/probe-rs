use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Chip detection method
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChipDetectionMethod {
    Atsam(AtsamDetection),
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
