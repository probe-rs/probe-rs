use crate::serialize::{hex_range, hex_u_int};
use core::ops::Range;
use serde::{Deserialize, Serialize};

/// Represents a region in non-volatile memory (e.g. flash or EEPROM).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NvmRegion {
    /// A name to describe the region
    pub name: Option<String>,
    /// Address range of the region
    #[serde(serialize_with = "hex_range")]
    pub range: Range<u64>,
    /// True if the chip boots from this memory
    #[serde(default)]
    pub is_boot_memory: bool,
    /// List of cores that can access this region
    pub cores: Vec<String>,
    /// True if the memory region is an alias of a different memory region.
    #[serde(default)]
    pub is_alias: bool,
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
    /// A name to describe the region
    pub name: Option<String>,
    /// Address range of the region
    #[serde(serialize_with = "hex_range")]
    pub range: Range<u64>,
    /// True if the chip boots from this memory
    #[serde(default)]
    pub is_boot_memory: bool,
    /// List of cores that can access this region
    pub cores: Vec<String>,
}

/// Represents a generic region.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GenericRegion {
    /// A name to describe the region
    pub name: Option<String>,
    /// Address range of the region
    #[serde(serialize_with = "hex_range")]
    pub range: Range<u64>,
    /// List of cores that can access this region
    pub cores: Vec<String>,
}

/// Holds information about a specific, individual flash
/// sector.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SectorInfo {
    /// Base address of the flash sector
    pub base_address: u64,
    /// Size of the flash sector
    pub size: u64,
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
    #[serde(serialize_with = "hex_u_int")]
    pub size: u64,
    /// Start address of the group of flash sectors, relative
    /// to the start address of the flash.
    #[serde(serialize_with = "hex_u_int")]
    pub address: u64,
}

/// Holds information about a page in flash.
#[derive(Debug, Copy, Clone)]
pub struct PageInfo {
    /// Base address of the page in flash.
    pub base_address: u64,
    /// Size of the page
    pub size: u32,
}

/// Holds information about the entire flash.
#[derive(Debug, Copy, Clone)]
pub struct NvmInfo {
    pub rom_start: u64,
}

/// Enables the user to do range intersection testing.
pub trait MemoryRange {
    /// Returns true if `self` contains `range` fully.
    fn contains_range(&self, range: &Range<u64>) -> bool;

    /// Returns true if `self` intersects `range` partially.
    fn intersects_range(&self, range: &Range<u64>) -> bool;

    /// Ensure memory reads using this memory range, will be aligned to 32 bits.
    /// This may result in slightly more memory being read than requested.
    fn align_to_32_bits(&mut self);
}

impl MemoryRange for Range<u64> {
    fn contains_range(&self, range: &Range<u64>) -> bool {
        if range.end == 0 {
            false
        } else {
            self.contains(&range.start) && self.contains(&(range.end - 1))
        }
    }

    fn intersects_range(&self, range: &Range<u64>) -> bool {
        if range.end == 0 {
            false
        } else {
            self.contains(&range.start) && !self.contains(&(range.end - 1))
                || !self.contains(&range.start) && self.contains(&(range.end - 1))
                || self.contains_range(range)
                || range.contains_range(self)
        }
    }

    fn align_to_32_bits(&mut self) {
        if self.start % 4 != 0 {
            self.start -= self.start % 4;
        }
        if self.end % 4 != 0 {
            // Try to align the end to 32 bits, but don't overflow.
            if let Some(new_end) = self.end.checked_add(4 - self.end % 4) {
                self.end = new_end;
            }
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

impl MemoryRegion {
    /// Get the cores to which this memory region belongs.
    pub fn cores(&self) -> &[String] {
        match self {
            MemoryRegion::Ram(region) => &region.cores,
            MemoryRegion::Generic(region) => &region.cores,
            MemoryRegion::Nvm(region) => &region.cores,
        }
    }
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

    #[test]
    fn test_align_to_32_bits_case1() {
        // Test case 1: start and end are already aligned
        let mut range = Range { start: 0, end: 8 };
        range.align_to_32_bits();
        assert_eq!(range.start, 0);
        assert_eq!(range.end, 8);
    }

    #[test]
    fn test_align_to_32_bits_case2() {
        // Test case 2: start is not aligned, end is aligned
        let mut range = Range { start: 3, end: 12 };
        range.align_to_32_bits();
        assert_eq!(range.start, 0);
        assert_eq!(range.end, 12);
    }

    #[test]
    fn test_align_to_32_bits_case3() {
        // Test case 3: start is aligned, end is not aligned
        let mut range = Range { start: 16, end: 23 };
        range.align_to_32_bits();
        assert_eq!(range.start, 16);
        assert_eq!(range.end, 24);
    }

    #[test]
    fn test_align_to_32_bits_case4() {
        // Test case 4: start and end are not aligned
        let mut range = Range { start: 5, end: 13 };
        range.align_to_32_bits();
        assert_eq!(range.start, 4);
        assert_eq!(range.end, 16);
    }
}
