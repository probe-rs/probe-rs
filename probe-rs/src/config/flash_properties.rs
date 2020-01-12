use super::memory::SectorDescription;
use derivative::Derivative;
use std::ops::Range;
#[derive(Debug, Derivative, Clone, Serialize, Deserialize)]
#[derivative(Default)]
pub struct FlashProperties {
    /// The range of the device flash.
    #[derivative(Default(value = "0..0"))]
    pub range: Range<u32>,
    /// The page size of the device flash.
    pub page_size: u32,
    /// The value of a byte in flash that was just erased.
    pub erased_byte_value: u8,
    /// The approximative time it takes to program a page.
    pub program_page_timeout: u32,
    /// The approximative time it takes to erase a sector.
    pub erase_sector_timeout: u32,
    /// The available sectors of the device flash.
    pub sectors: Vec<SectorDescription>,
}

// impl FlashProperties {
//     pub fn address_range(&self) -> Range<u32> {
//         self.start_address..(self.start_address + self.size)
//     }
// }
