bitflags! {
    pub struct Access: u8 {
        const R = 0b00000001;
        const W = 0b00000010;
        const X = 0b00000100;
        const RW = Self::R.bits | Self::W.bits;
        const RX = Self::R.bits | Self::X.bits;
    }
}

pub const PROGRAM_PAGE_WEIGHT: f32 = 0.130;
pub const ERASE_SECTOR_WEIGHT: f32 = 0.048;
pub const ERASE_ALL_WEIGHT: f32 = 0.174;

#[derive(Debug, Clone)]
pub struct FlashRegion {
    pub range: core::ops::Range<u32>,
    pub is_boot_memory: bool,
    pub is_testable: bool,
    pub blocksize: u32,
    pub sector_size: u32,
    pub page_size: u32,
    pub phrase_size: u32,
    pub erase_all_weight: f32,
    pub erase_sector_weight: f32,
    pub program_page_weight: f32,
    pub erased_byte_value: u8,
    pub access: Access,
    pub are_erased_sectors_readable: bool,
}

#[derive(Debug, Clone)]
pub struct RamRegion {
    pub range: core::ops::Range<u32>,
    pub is_boot_memory: bool,
    pub is_testable: bool,
}

#[derive(Debug, Clone)]
pub struct RomRegion {
    pub range: core::ops::Range<u32>,
}

#[derive(Debug, Clone)]
pub struct DeviceRegion {
    pub range: core::ops::Range<u32>,
}

pub struct SectorInfo {
    pub base_address: u32,
    pub erase_weight: f32,
    pub size: u32,
}

pub struct PageInfo {
    pub base_address: u32,
    pub program_weight: f32,
    pub size: u32,
}

pub struct FlashInfo {
    pub rom_start: u32,
    pub erase_weight: f32,
    pub crc_supported: bool,
}

pub trait MemoryRange {
    fn contains_range(&self, range: &std::ops::Range<u32>) -> bool;
    fn intersects_range(&self, range: &std::ops::Range<u32>) -> bool;
}

impl MemoryRange for core::ops::Range<u32> {
    fn contains_range(&self, range: &std::ops::Range<u32>) -> bool {
        self.contains(&range.start) && self.contains(&(range.end - 1))
    }

    fn intersects_range(&self, range: &std::ops::Range<u32>) -> bool {
        self.contains(&range.start) && !self.contains(&(range.end - 1))
     || !self.contains(&range.start) && self.contains(&(range.end - 1))
    }
}

#[derive(Clone)]
pub enum MemoryRegion {
    Ram(RamRegion),
    Rom(RomRegion),
    Flash(FlashRegion),
    Device(DeviceRegion)
}