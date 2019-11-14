use jep106::JEP106Code;
use super::variant::Variant;

/// This describes a single chip model.
/// It can come in different configurations (memory, peripherals).
/// E.g. `nRF52832` is a `Chip` where `nRF52832_xxAA` and `nRF52832_xxBB` are its `Variant`s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chip {
    /// This is the name of the chip in base form.
    /// E.g. `nRF52832`.
    pub name: String,
    /// The JEP106 code of the manufacturer.
    pub manufacturer: JEP106Code,
    /// The `PART` register of the chip.
    /// This value can be determined via the `cli info` command.
    pub part: u32,
    /// The name of the flash algorithm.
    pub flash_algorithm: String,
    /// A list of available variants of the chip.
    pub variants: Vec<Variant>,
    /// The name of the core type.
    /// E.g. `M0` or `M4`.
    pub core: String,
}