use super::memory::MemoryRegion;
use std::borrow::Cow;

/// This describes a single chip model.
/// It can come in different configurations (memory, peripherals).
/// E.g. `nRF52832` is a `Chip` where `nRF52832_xxAA` and `nRF52832_xxBB` are its `Variant`s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chip {
    /// This is the name of the chip in base form.
    /// E.g. `nRF52832`.
    pub name: Cow<'static, str>,
    /// The `PART` register of the chip.
    /// This value can be determined via the `cli info` command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<u16>,
    /// The memory regions available on the chip.
    pub memory_map: Cow<'static, [MemoryRegion]>,

    pub flash_algorithms: Cow<'static, [Cow<'static, str>]>,
}
