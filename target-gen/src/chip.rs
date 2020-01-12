use probe_rs::config::memory::MemoryRegion;
use serde::{Deserialize, Serialize};

/// This describes a single chip model.
/// It can come in different configurations (memory, peripherals).
/// E.g. `nRF52832` is a `Chip` where `nRF52832_xxAA` and `nRF52832_xxBB` are its `Variant`s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chip {
    /// This is the name of the chip in base form.
    /// E.g. `nRF52832`.
    pub name: String,
    /// The name of the flash algorithm.
    pub memory_map: Vec<MemoryRegion>,

    pub flash_algorithms: Vec<String>,
}
