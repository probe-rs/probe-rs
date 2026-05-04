use super::memory::{PageInfo, SectorDescription, SectorInfo};
use crate::serialize::{hex_range, hex_u_int};
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// Properties of flash memory, which
/// are used when programming Flash memory.
///
/// These values are read from the
/// YAML target description files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FlashProperties {
    /// The range of the device flash.
    #[serde(serialize_with = "hex_range")]
    pub address_range: Range<u64>,
    /// The page size of the device flash.
    #[serde(serialize_with = "hex_u_int")]
    pub page_size: u32,
    /// The value of a byte in flash that was just erased.
    #[serde(serialize_with = "hex_u_int")]
    pub erased_byte_value: u8,
    /// The approximate time it takes to program a page.
    pub program_page_timeout: u32,
    /// The approximate time it takes to erase a sector.
    pub erase_sector_timeout: u32,
    /// The available sectors of the device flash.
    #[serde(default)]
    pub sectors: Vec<SectorDescription>,
}

impl FlashProperties {
    /// Returns information about the sector that contains `address`, or `None` if the address
    /// is outside the flash range.
    pub fn sector_info(&self, address: u64) -> Option<SectorInfo> {
        if !self.address_range.contains(&address) {
            return None;
        }

        let offset_address = address - self.address_range.start;

        let containing_sector = self.sectors.iter().rfind(|s| s.address <= offset_address)?;

        let sector_index = (offset_address - containing_sector.address) / containing_sector.size;

        let sector_address = self.address_range.start
            + containing_sector.address
            + sector_index * containing_sector.size;

        Some(SectorInfo {
            base_address: sector_address,
            size: containing_sector.size,
        })
    }

    /// Returns information about the page that contains `address`, or `None` if the address
    /// is outside the flash range.
    pub fn page_info(&self, address: u64) -> Option<PageInfo> {
        if !self.address_range.contains(&address) {
            return None;
        }

        Some(PageInfo {
            base_address: address - (address % self.page_size as u64),
            size: self.page_size,
        })
    }

    /// Iterates over all sectors in address order.
    pub fn iter_sectors(&self) -> impl Iterator<Item = SectorInfo> + '_ {
        assert!(!self.sectors.is_empty());
        assert!(self.sectors[0].address == 0);

        let mut addr = self.address_range.start;
        let mut desc_idx = 0;
        std::iter::from_fn(move || {
            if addr >= self.address_range.end {
                return None;
            }

            if let Some(next_desc) = self.sectors.get(desc_idx + 1)
                && self.address_range.start + next_desc.address <= addr
            {
                desc_idx += 1;
            }

            let size = self.sectors[desc_idx].size;
            let sector = SectorInfo {
                base_address: addr,
                size,
            };
            addr += size;
            Some(sector)
        })
    }

    /// Iterates over all pages in address order.
    pub fn iter_pages(&self) -> impl Iterator<Item = PageInfo> + '_ {
        let mut addr = self.address_range.start;
        std::iter::from_fn(move || {
            if addr >= self.address_range.end {
                return None;
            }

            let page = PageInfo {
                base_address: addr,
                size: self.page_size,
            };
            addr += self.page_size as u64;
            Some(page)
        })
    }
}

impl Default for FlashProperties {
    fn default() -> Self {
        FlashProperties {
            address_range: 0..0,
            page_size: 0,
            erased_byte_value: 0,
            program_page_timeout: 0,
            erase_sector_timeout: 0,
            sectors: vec![],
        }
    }
}
