use super::memory::MemoryRegion;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Variant {
    /// The variant name of the chip.
    /// E.g. `nRF52832_xxAA`, `nRF52832_xxBB`
    pub name: String,
    pub memory_map: Vec<MemoryRegion>,
}