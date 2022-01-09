use scroll::Pread;

use anyhow::{anyhow, Result};

/// A struct to describe one sector in Flash.
#[derive(Clone, Debug)]
pub(crate) struct SectorInfo {
    pub address: u32,
    pub size: u32,
}

impl SectorInfo {
    // This value signalizes the end of a sector.
    const SECTOR_END: u32 = 0xFFFF_FFFF;

    /// Creates a new `SectorInfo` struct for a chunk of ELF data.
    fn new(data: &[u8]) -> Option<Self> {
        let size = data.pread(0).unwrap();
        let address = data.pread(4).unwrap();
        if size != Self::SECTOR_END && address != Self::SECTOR_END {
            Some(Self { address, size })
        } else {
            None
        }
    }
}

/// This struct describes the flash algorithm.
/// It can be parsed from an ELF symbol.
///
/// You should always use `FlashDevice::new()` to create this struct.
///
// This struct takes 160 bytes + the size of all sectors at the end in the ELF binary.
// The data types of this struct represent the actual size they have in the C struct too!
#[derive(Clone, Debug)]
pub(crate) struct FlashDevice {
    /// The flash algorithm version.
    pub(crate) _driver_version: u16,
    /// The name of the device.
    pub(crate) name: String, // Max 128 bytes in size
    /// The type of flash algorithm (MORE INFO REQUIRED).
    pub(crate) _typ: u16,
    /// The flash start address.
    pub(crate) start_address: u32,
    /// The flash size in bytes.
    pub(crate) device_size: u32,
    /// The flash page size in bytes.
    pub(crate) page_size: u32,
    _reserved: u32,
    /// The default erased value of one byte in flash.
    pub(crate) erased_default_value: u8,
    //  _pad: u24,
    // (MORE INFO REQUIRED)
    pub(crate) program_page_timeout: u32, // in miliseconds
    // (MORE INFO REQUIRED)
    pub(crate) erase_sector_timeout: u32, // in miliseconds
    // The available sectors of the flash.
    pub(crate) sectors: Vec<SectorInfo>,
}

impl FlashDevice {
    const INFO_SIZE: u32 = 160;
    const SECTOR_INFO_SIZE: u32 = 8;
    const MAX_ID_STRING_LENGTH: usize = 128;

    /// Parses the `FlashDevice` struct from ELF binary data.
    pub(crate) fn new(elf: &goblin::elf::Elf<'_>, buffer: &[u8], address: u32) -> Result<Self> {
        // Extract all the sector data from the ELF blob.
        let sectors = Self::parse_sectors(elf, buffer, address);

        // Get the rest of the data stored in the struct.
        let data = crate::parser::read_elf_bin_data(elf, buffer, address, Self::INFO_SIZE)
            .ok_or_else(|| anyhow!("Failed to read binary data for flash device. Read address: {:#010x}, size: {} bytes", address, Self::INFO_SIZE))?;

        // Get the string length of the name
        let hypothetical_length = data[2..2 + Self::MAX_ID_STRING_LENGTH]
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(Self::MAX_ID_STRING_LENGTH);
        let sanitized_length = Self::MAX_ID_STRING_LENGTH.min(hypothetical_length);

        // Finally parse the struct data and return the struct.
        Ok(Self {
            _driver_version: data.pread(0).unwrap(),
            name: String::from_utf8_lossy(&data[2..2 + sanitized_length]).to_string(),
            _typ: data.pread(130).unwrap(),
            start_address: data.pread(132).unwrap(),
            device_size: data.pread(136).unwrap(),
            page_size: data.pread(140).unwrap(),
            _reserved: data.pread(144).unwrap(),
            erased_default_value: data.pread(148).unwrap(),
            program_page_timeout: data.pread(152).unwrap(),
            erase_sector_timeout: data.pread(156).unwrap(),
            sectors,
        })
    }

    /// Parse the sector infos in the device struct.
    pub(crate) fn parse_sectors(
        elf: &goblin::elf::Elf<'_>,
        buffer: &[u8],
        address: u32,
    ) -> Vec<SectorInfo> {
        let mut sectors = vec![];
        let mut offset = Self::INFO_SIZE;
        // As long as we find new sectors, keep em comming.
        while let Some(data) =
            crate::parser::read_elf_bin_data(elf, buffer, address + offset, Self::SECTOR_INFO_SIZE)
        {
            if let Some(sector) = SectorInfo::new(data) {
                sectors.push(sector);
                offset += 8;
            } else {
                break;
            }
        }

        sectors
    }
}
