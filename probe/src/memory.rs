#[derive(Debug, Default, Copy, Clone)]
pub struct MemoryRegion {
    pub start: u32,
    pub page_size: u32,
    pub sector_size: u32,
    pub erase_sector_weight: u32,
    pub program_page_weight: u32,
    pub erase_all_weight: u32,
}

impl MemoryRegion {
    pub fn contrains_address(&self, address: u32) -> bool {
        unimplemented!();
    }
}

pub struct SectorInfo {
    pub base_address: u32,
    pub erase_weight: u32,
    pub size: u32,
}

pub struct PageInfo {
    pub base_address: u32,
    pub program_weight: u32,
    pub size: u32,
}

pub struct FlashInfo {
    pub rom_start: u32,
    pub erase_weight: u32,
    pub crc_supported: bool,
}