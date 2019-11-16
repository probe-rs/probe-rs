use probe_rs::config::memory::MemoryRegion;
use probe_rs::config::flash_algorithm::FlashAlgorithm;
use serde::{Serialize, Deserialize};

/// This describes a single chip model.
/// It can come in different configurations (memory, peripherals).
/// E.g. `nRF52832` is a `Chip` where `nRF52832_xxAA` and `nRF52832_xxBB` are its `Variant`s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chip {
    /// This is the name of the chip in base form.
    /// E.g. `nRF52832`.
    pub name: String,
    /// The name of the flash algorithm.
    pub flash_algorithms: Vec<FlashAlgorithm>,
    /// The memory regions available on the chip.
    pub memory_map: Vec<MemoryRegion>,
    /// The name of the core type.
    /// E.g. `M0` or `M4`.
    pub core: String,
}