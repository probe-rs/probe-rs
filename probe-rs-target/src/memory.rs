use core::ops::Range;
use serde::{Deserialize, Serialize};

/// Represents a region in non-volatile memory (e.g. flash or EEPROM).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NvmRegion {
    /// Address range of the region
    pub range: Range<u32>,
    /// True if the chip boots from this memory
    pub is_boot_memory: bool,
}

impl NvmRegion {
    /// Returns the necessary information about the NVM.
    pub fn nvm_info(&self) -> NvmInfo {
        NvmInfo {
            rom_start: self.range.start,
        }
    }
}

/// Represents a region in RAM.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RamRegion {
    /// Address range of the region
    pub range: Range<u32>,
    /// True if the chip boots from this memory
    pub is_boot_memory: bool,
}

/// Represents a generic region.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GenericRegion {
    /// Address range of the region
    pub range: Range<u32>,
}

/// Holds information about a specific, individual flash
/// sector.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct SectorInfo {
    /// Base address of the flash sector
    pub base_address: u32,
    /// Size of the flash sector
    pub size: u32,
}

/// Information about a group of flash sectors, which
/// is used as part of the [`FlashProperties`] struct.
///
/// The SectorDescription means that, starting at the
/// flash address `address`, all following sectors will
/// have a size of `size`. This is valid until either the
/// end of the flash, or until another `SectorDescription`
/// changes the sector size.
///
/// [`FlashProperties`]: crate::FlashProperties
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SectorDescription {
    /// Size of each individual flash sector
    pub size: u32,
    /// Start address of the group of flash sectors, relative
    /// to the start address of the flash.
    pub address: u32,
}

/// Holds information about a page in flash.
#[derive(Debug, Copy, Clone)]
pub struct PageInfo {
    /// Base address of the page in flash.
    pub base_address: u32,
    /// Size of the page
    pub size: u32,
}

/// Holds information about the entire flash.
#[derive(Debug, Copy, Clone)]
pub struct NvmInfo {
    pub rom_start: u32,
}

/// Enables the user to do range intersection testing.
pub trait MemoryRange {
    /// Returns true if `self` contains `range` fully.
    fn contains_range(&self, range: &Range<u32>) -> bool;

    /// Returns true if `self` intersects `range` partially.
    fn intersects_range(&self, range: &Range<u32>) -> bool;
}

impl MemoryRange for Range<u32> {
    fn contains_range(&self, range: &Range<u32>) -> bool {
        if range.end == 0 {
            false
        } else {
            self.contains(&range.start) && self.contains(&(range.end - 1))
        }
    }

    fn intersects_range(&self, range: &Range<u32>) -> bool {
        if range.end == 0 {
            false
        } else {
            self.contains(&range.start) && !self.contains(&(range.end - 1))
                || !self.contains(&range.start) && self.contains(&(range.end - 1))
                || self.contains_range(range)
                || range.contains_range(self)
        }
    }
}

/// Declares the type of a memory region.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryRegion {
    /// Memory region describing RAM.
    Ram(RamRegion),
    /// Generic memory region, which is neither
    /// flash nor RAM.
    Generic(GenericRegion),
    /// Memory region describing flash, EEPROM or other non-volatile memory.
    #[serde(alias = "Flash")] // Keeping the "Flash" name this for backwards compatibility
    Nvm(NvmRegion),
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
