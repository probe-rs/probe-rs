use crate::Chip;
use probe_rs::config::flash_algorithm::RawFlashAlgorithm;
use serde::{Serialize, Deserialize};

/// This describes a chip family with all its variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChipFamily {
    /// This is the name of the chip family in base form.
    /// E.g. `nRF52832`.
    pub(crate) name: String,
    /// This vector holds all the variants of the family.
    pub(crate) variants: Vec<Chip>,
    /// This vector holds all available algorithms.
    pub(crate) flash_algorithms: Vec<RawFlashAlgorithm>,
    /// The name of the core type.
    /// E.g. `M0` or `M4`.
    pub(crate) core: String,
}

impl ChipFamily {
    /// Create a new `ChipFamily`.
    pub(crate) fn new(name: String, flash_algorithms: Vec<RawFlashAlgorithm>, core: String) -> Self {
        Self {
            name,
            variants: Vec::new(),
            flash_algorithms,
            core,
        }
    }
}