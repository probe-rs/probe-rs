bitflags! {
    #[derive(Serialize, Deserialize)]
    pub struct Access: u8 {
        const R = 0b00000001;
        const W = 0b00000010;
        const X = 0b00000100;
        const RW = Self::R.bits | Self::W.bits;
        const RX = Self::R.bits | Self::X.bits;
    }
}

mod integer_representation {
    use serde::{self, Deserialize, Deserializer, Serialize, Serializer};
    
    // CHANGE THIS ACCORDING TO YOUR CODE
    use super::Access; 
    type IntRep = u8;
    type Flags = Access;
    
    pub fn serialize<S>(date: &Flags, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {   
        date.bits().serialize(serializer)
    }
    
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Flags, D::Error>
    where
        D: Deserializer<'de>,
    {   
        let raw: IntRep = IntRep::deserialize(deserializer)?;
        Access::from_bits(raw).ok_or(serde::de::Error::custom(format!(
            "Unexpected flags value {}",
            raw              
        )))                  
    }                  
} 

pub const PROGRAM_PAGE_WEIGHT: f32 = 0.130;
pub const ERASE_SECTOR_WEIGHT: f32 = 0.048;
pub const ERASE_ALL_WEIGHT: f32 = 0.174;

#[derive(Derivative, Clone, Serialize, Deserialize)]
#[derivative(Debug, PartialEq, Eq, Hash)]
pub struct FlashRegion {
    pub range: core::ops::Range<u32>,
    pub is_boot_memory: bool,
    pub is_testable: bool,
    pub blocksize: u32,
    pub sector_size: u32,
    pub page_size: u32,
    pub phrase_size: u32,
    #[derivative(PartialEq="ignore")]
    #[derivative(Hash="ignore")]
    pub erase_all_weight: f32,
    #[derivative(PartialEq="ignore")]
    #[derivative(Hash="ignore")]
    pub erase_sector_weight: f32,
    #[derivative(PartialEq="ignore")]
    #[derivative(Hash="ignore")]
    pub program_page_weight: f32,
    pub erased_byte_value: u8,
    #[serde(with = "integer_representation")]
    pub access: Access,
    pub are_erased_sectors_readable: bool,
}

impl FlashRegion {
    pub fn get_sector_info(&self, address: u32) -> Option<SectorInfo> {
        if !self.range.contains(&address) {
            return None
        }

        Some(SectorInfo {
            base_address: address - (address % self.sector_size),
            erase_weight: self.erase_sector_weight,
            size: self.sector_size,
        })
    }

    pub fn get_page_info(&self, address: u32) -> Option<PageInfo> {
        if !self.range.contains(&address) {
            return None
        }

        Some(PageInfo {
            base_address: address - (address % self.page_size),
            program_weight: self.program_page_weight,
            size: self.page_size,
        })
    }

    pub fn get_flash_info(&self, analyzer_supported: bool) -> FlashInfo {
        FlashInfo {
            rom_start: self.range.start,
            erase_weight: self.erase_all_weight,
            crc_supported: analyzer_supported,
        }
    }

    pub fn is_erased(&self, data: &[u8]) -> bool {
        for b in data {
            if *b != self.erased_byte_value {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RamRegion {
    pub range: core::ops::Range<u32>,
    pub is_boot_memory: bool,
    pub is_testable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RomRegion {
    pub range: core::ops::Range<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryRegion {
    Ram(RamRegion),
    Rom(RomRegion),
    Flash(FlashRegion),
    Device(DeviceRegion)
}