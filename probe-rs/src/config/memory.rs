use core::ops::Range;

/// Represents a region in flash.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FlashRegion {
    pub range: Range<u32>,
    pub is_boot_memory: bool,
    pub page_size: u32,
    pub sector_size: u32,
    pub erased_byte_value: u8,
}

impl FlashRegion {
    /// Returns the necessary information about the page which `address` resides in
    /// if the address is inside the flash region.
    pub fn page_info(&self, address: u32) -> Option<PageInfo> {
        if !self.range.contains(&address) {
            return None;
        }

        Some(PageInfo {
            base_address: address - (address % self.page_size),
            size: self.page_size,
        })
    }

    /// Returns the necessary information about the flash.
    pub fn flash_info(&self) -> FlashInfo {
        FlashInfo {
            rom_start: self.range.start,
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
    pub range: Range<u32>,
    pub is_boot_memory: bool,
}

/// Represents a generic region.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GenericRegion {
    pub range: Range<u32>,
}

/// Holds information about a sepcific flash sector.
#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct SectorInfo {
    pub base_address: u32,
    pub page_size: u32,
    pub size: u32,
}

/// Holds information about a sepcific flash sector.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SectorDescription {
    pub size: u32,
    pub count: u32,
}

impl SectorDescription {
    pub fn total_size(&self) -> u32 {
        self.size * self.count
    }
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
}

/// Enables the user to do range intersection testing.
pub trait MemoryRange {
    fn contains_range(&self, range: &Range<u32>) -> bool;
    fn intersects_range(&self, range: &Range<u32>) -> bool;
}

impl MemoryRange for Range<u32> {
    /// Returns true if `self` contains `range` fully.
    fn contains_range(&self, range: &Range<u32>) -> bool {
        self.contains(&range.start) && self.contains(&(range.end - 1))
    }

    /// Returns true if `self` intersects `range` partially.
    fn intersects_range(&self, range: &Range<u32>) -> bool {
        self.contains(&range.start) && !self.contains(&(range.end - 1))
            || !self.contains(&range.start) && self.contains(&(range.end - 1))
            || self.contains_range(range)
            || range.contains_range(self)
    }
}

/// Declares the type of a memory region.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryRegion {
    Ram(RamRegion),
    Generic(GenericRegion),
    Flash(FlashRegion),
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn contains_range1() {
        let range1 = 0..1;
        let range2 = 0..1;
        assert!(range1.contains_range(&range2));
    }

    #[test]
    fn contains_range2() {
        let range1 = 0..1;
        let range2 = 0..2;
        assert!(!range1.contains_range(&range2));
    }

    #[test]
    fn contains_range3() {
        let range1 = 0..4;
        let range2 = 0..1;
        assert!(range1.contains_range(&range2));
    }

    #[test]
    fn contains_range4() {
        let range1 = 4..8;
        let range2 = 3..9;
        assert!(!range1.contains_range(&range2));
    }

    #[test]
    fn contains_range5() {
        let range1 = 4..8;
        let range2 = 0..1;
        assert!(!range1.contains_range(&range2));
    }

    #[test]
    fn contains_range6() {
        let range1 = 4..8;
        let range2 = 6..8;
        assert!(range1.contains_range(&range2));
    }

    #[test]
    fn intersects_range1() {
        let range1 = 0..1;
        let range2 = 0..1;
        assert!(range1.intersects_range(&range2));
    }

    #[test]
    fn intersects_range2() {
        let range1 = 0..1;
        let range2 = 0..2;
        assert!(range1.intersects_range(&range2));
    }

    #[test]
    fn intersects_range3() {
        let range1 = 0..4;
        let range2 = 0..1;
        assert!(range1.intersects_range(&range2));
    }

    #[test]
    fn intersects_range4() {
        let range1 = 4..8;
        let range2 = 3..9;
        assert!(range1.intersects_range(&range2));
    }

    #[test]
    fn intersects_range5() {
        let range1 = 4..8;
        let range2 = 0..1;
        assert!(!range1.intersects_range(&range2));
    }

    #[test]
    fn intersects_range6() {
        let range1 = 4..8;
        let range2 = 6..8;
        assert!(range1.intersects_range(&range2));
    }

    #[test]
    fn intersects_range7() {
        let range1 = 4..8;
        let range2 = 3..4;
        assert!(!range1.intersects_range(&range2));
    }

    #[test]
    fn intersects_range8() {
        let range1 = 8..9;
        let range2 = 6..8;
        assert!(!range1.intersects_range(&range2));
    }

    #[test]
    fn intersects_range9() {
        let range1 = 2..4;
        let range2 = 6..8;
        assert!(!range1.intersects_range(&range2));
    }
}
