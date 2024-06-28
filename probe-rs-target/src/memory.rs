use crate::serialize::{hex_range, hex_u_int};
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// Represents a region in non-volatile memory (e.g. flash or EEPROM).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NvmRegion {
    /// A name to describe the region
    pub name: Option<String>,
    /// Address range of the region
    #[serde(serialize_with = "hex_range")]
    pub range: Range<u64>,
    /// List of cores that can access this region
    pub cores: Vec<String>,
    /// True if the memory region is an alias of a different memory region.
    #[serde(default)]
    pub is_alias: bool,
    /// Access permissions for the region.
    #[serde(default)]
    pub access: Option<MemoryAccess>,
}

impl NvmRegion {
    /// Returns whether the region is accessible by the given core.
    pub fn accessible_by(&self, core_name: &str) -> bool {
        self.cores.iter().any(|c| c == core_name)
    }

    /// Returns the access permissions for the region.
    pub fn access(&self) -> MemoryAccess {
        self.access.unwrap_or_default()
    }

    /// Returns whether the region is readable.
    pub fn is_readable(&self) -> bool {
        self.access().read
    }

    /// Returns whether the region is writable.
    pub fn is_writable(&self) -> bool {
        self.access().write
    }

    /// Returns whether the region is executable.
    pub fn is_executable(&self) -> bool {
        self.access().execute
    }

    /// Returns whether the region is boot memory.
    pub fn is_boot_memory(&self) -> bool {
        self.access().boot
    }
}

impl NvmRegion {
    /// Returns the necessary information about the NVM.
    pub fn nvm_info(&self) -> NvmInfo {
        NvmInfo {
            rom_start: self.range.start,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Represents access permissions of a region in RAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MemoryAccess {
    /// True if the region is readable.
    #[serde(default = "default_true")]
    pub read: bool,
    /// True if the region is writable.
    #[serde(default = "default_true")]
    pub write: bool,
    /// True if the region is executable.
    #[serde(default = "default_true")]
    pub execute: bool,
    /// True if the chip boots from this memory
    #[serde(default)]
    pub boot: bool,
}

impl Default for MemoryAccess {
    fn default() -> Self {
        MemoryAccess {
            read: true,
            write: true,
            execute: true,
            boot: false,
        }
    }
}

/// Represents a region in RAM.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RamRegion {
    /// A name to describe the region
    pub name: Option<String>,
    /// Address range of the region
    #[serde(serialize_with = "hex_range")]
    pub range: Range<u64>,
    /// List of cores that can access this region
    pub cores: Vec<String>,
    /// Access permissions for the region.
    #[serde(default)]
    pub access: Option<MemoryAccess>,
}

impl RamRegion {
    /// Returns whether the region is accessible by the given core.
    pub fn accessible_by(&self, core_name: &str) -> bool {
        self.cores.iter().any(|c| c == core_name)
    }

    /// Returns the access permissions for the region.
    pub fn access(&self) -> MemoryAccess {
        self.access.unwrap_or_default()
    }

    /// Returns whether the region is readable.
    pub fn is_readable(&self) -> bool {
        self.access().read
    }

    /// Returns whether the region is writable.
    pub fn is_writable(&self) -> bool {
        self.access().write
    }

    /// Returns whether the region is executable.
    pub fn is_executable(&self) -> bool {
        self.access().execute
    }

    /// Returns whether the region is boot memory.
    pub fn is_boot_memory(&self) -> bool {
        self.access().boot
    }
}

/// Represents a generic region.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenericRegion {
    /// A name to describe the region
    pub name: Option<String>,
    /// Address range of the region
    #[serde(serialize_with = "hex_range")]
    pub range: Range<u64>,
    /// List of cores that can access this region
    pub cores: Vec<String>,
    /// Access permissions for the region.
    #[serde(default)]
    pub access: Option<MemoryAccess>,
}

impl GenericRegion {
    /// Returns whether the region is accessible by the given core.
    pub fn accessible_by(&self, core_name: &str) -> bool {
        self.cores.iter().any(|c| c == core_name)
    }

    /// Returns the access permissions for the region.
    pub fn access(&self) -> MemoryAccess {
        self.access.unwrap_or_default()
    }

    /// Returns whether the region is readable.
    pub fn is_readable(&self) -> bool {
        self.access().read
    }

    /// Returns whether the region is writable.
    pub fn is_writable(&self) -> bool {
        self.access().write
    }

    /// Returns whether the region is executable.
    pub fn is_executable(&self) -> bool {
        self.access().execute
    }
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

impl SectorInfo {
    /// Returns the address range of the sector.
    pub fn address_range(&self) -> Range<u64> {
        self.base_address..self.base_address + self.size
    }
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

impl PageInfo {
    /// Returns the address range of the sector.
    pub fn address_range(&self) -> Range<u64> {
        self.base_address..self.base_address + self.size as u64
    }
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
    /// Returns the RAM region if this is a RAM region, otherwise None.
    pub fn as_ram_region(&self) -> Option<&RamRegion> {
        match self {
            MemoryRegion::Ram(region) => Some(region),
            _ => None,
        }
    }

    /// Returns the NVM region if this is a NVM region, otherwise None.
    pub fn as_nvm_region(&self) -> Option<&NvmRegion> {
        match self {
            MemoryRegion::Nvm(region) => Some(region),
            _ => None,
        }
    }

    /// Returns the address range of the memory region.
    pub fn address_range(&self) -> Range<u64> {
        match self {
            MemoryRegion::Ram(rr) => rr.range.clone(),
            MemoryRegion::Generic(gr) => gr.range.clone(),
            MemoryRegion::Nvm(nr) => nr.range.clone(),
        }
    }

    /// Returns whether the memory region contains the given address.
    pub fn contains(&self, address: u64) -> bool {
        self.address_range().contains(&address)
    }

    /// Get the cores to which this memory region belongs.
    pub fn cores(&self) -> &[String] {
        match self {
            MemoryRegion::Ram(region) => &region.cores,
            MemoryRegion::Generic(region) => &region.cores,
            MemoryRegion::Nvm(region) => &region.cores,
        }
    }

    /// Returns `true` if the memory region is [`Ram`].
    ///
    /// [`Ram`]: MemoryRegion::Ram
    #[must_use]
    pub fn is_ram(&self) -> bool {
        matches!(self, Self::Ram(..))
    }

    /// Returns `true` if the memory region is [`Nvm`].
    ///
    /// [`Nvm`]: MemoryRegion::Nvm
    #[must_use]
    pub fn is_nvm(&self) -> bool {
        matches!(self, Self::Nvm(..))
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
