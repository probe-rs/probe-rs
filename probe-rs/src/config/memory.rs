/// Represents a region in flash.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FlashRegion {
    pub range: core::ops::Range<u32>,
    pub is_boot_memory: bool,
    pub sector_size: u32,
    pub page_size: u32,
    pub erased_byte_value: u8,
}

impl FlashRegion {
    /// Returns the necessary information about the sector which address resides in
    /// if the address is inside the flash region.
    pub fn get_sector_info(&self, address: u32) -> Option<SectorInfo> {
        if !self.range.contains(&address) {
            return None;
        }

        Some(SectorInfo {
            base_address: address - (address % self.sector_size),
            size: self.sector_size,
        })
    }
    
    /// Returns the necessary information about the page which address resides in
    /// if the address is inside the flash region.
    pub fn get_page_info(&self, address: u32) -> Option<PageInfo> {
        if !self.range.contains(&address) {
            return None;
        }

        Some(PageInfo {
            base_address: address - (address % self.page_size),
            size: self.page_size,
        })
    }

    /// Returns the necessary information about the flash.
    pub fn get_flash_info(&self, analyzer_supported: bool) -> FlashInfo {
        FlashInfo {
            rom_start: self.range.start,
            crc_supported: analyzer_supported,
        }
    }

    /// Returns true if the entire contents of the argument array equal the erased byte value.
    pub fn is_erased(&self, data: &[u8]) -> bool {
        for b in data {
            if *b != self.erased_byte_value {
                return false;
            }
        }
        true
    }
}

/// Represents a region in RAM.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RamRegion {
    pub range: core::ops::Range<u32>,
    pub is_boot_memory: bool,
    pub is_testable: bool,
}

/// Represents a generic region.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GenericRegion {
    pub range: core::ops::Range<u32>,
}

/// Holds information about a flash sector.
#[derive(Debug, Copy, Clone)]
pub struct SectorInfo {
    pub base_address: u32,
    pub size: u32,
}

/// Holds information about a page in flash.
#[derive(Debug, Copy, Clone)]
pub struct PageInfo {
    pub base_address: u32,
    pub size: u32,
}

/// Holds information about the entire flash.
#[derive(Debug, Copy, Clone)]
pub struct FlashInfo {
    pub rom_start: u32,
    pub crc_supported: bool,
}

/// Enables the user to do range intersection testing.
pub trait MemoryRange {
    fn contains_range(&self, range: &std::ops::Range<u32>) -> bool;
    fn intersects_range(&self, range: &std::ops::Range<u32>) -> bool;
}

impl MemoryRange for core::ops::Range<u32> {
    /// Returns true if `self` contains `range` fully.
    fn contains_range(&self, range: &std::ops::Range<u32>) -> bool {
        self.contains(&range.start) && self.contains(&(range.end - 1))
    }

    /// Returns true if `self` intersects `range` partially.
    fn intersects_range(&self, range: &std::ops::Range<u32>) -> bool {
        self.contains(&range.start) && !self.contains(&(range.end - 1))
    || !self.contains(&range.start) && self.contains(&(range.end - 1))
    }
}

/// Decalares the type of a memory region.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryRegion {
    Ram(RamRegion),
    Generic(GenericRegion),
    Flash(FlashRegion),
}
